//! The Stop legs: the session-state report, the target stop decision, the
//! context nudge, and the operator-capture depth.
//!
//! The context nudge was hooks/context-nudge.sh. EVERY session gets the
//! context check: past the trigger it blocks once per 10% band. A teamed lead
//! is nudged earlier (lead_used_pct_trigger, default 40) than any other
//! session (used_pct_trigger, default 50), and what the trigger asks for is a
//! COMPACT: a team survives one, and a successor gets a NEW mail handle that
//! orphans every worker still holding the old one. A teamed lead also gets
//! the orphan check (live workers it spawned, no resolution recorded), and
//! every session gets the flush check (a dirty tree static across turn-ends).
//!
//! Every gating signal is handed to the leg in its payload (transcript_path,
//! session_id) or read from live external state at fire time: the probe, the
//! registry, the carveout ledger, config. No pid-liveness probe, no read of
//! the session manifest. The probe is the one pressure source; the registry
//! is best-effort and only picks the lead trigger, the lead message and the
//! orphan check. Every error path answers 0 with no output: the check
//! degrades toward silence, never toward a false handoff.
//!
//! Block is the only Stop output documented to reach the model on Claude and
//! Codex, so each check blocks and records an event row that
//! `fno doctor event audit` reads to prove the leg fired.

use super::{Leg, CLAUDE_DEFAULT_SECS};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) fn legs() -> Vec<Leg> {
    vec![
        Leg::new("session-state", 5, session_state),
        Leg::new("target-stop-hook", CLAUDE_DEFAULT_SECS, target_stop_hook),
        Leg::new("context-nudge", CLAUDE_DEFAULT_SECS, context_nudge),
        Leg::new("operator-capture-nudge", 4, operator_capture_nudge),
    ]
}

fn session_state(_args: &[String]) -> i32 {
    if super::mux_profile() {
        super::session_state_leg("Stop")
    } else {
        0
    }
}

fn target_stop_hook(_args: &[String]) -> i32 {
    crate::hook::stop::run(&[])
}

/// The detached cache refresher re-enters a leg with this first argument:
/// `--cache-refresh <file> <fingerprint> <cmd> [args...]`.
const CACHE_REFRESH: &str = "--cache-refresh";

/// Window gate: below this many window tokens only the capacity branch fires.
const MIN_WINDOW: u64 = 400_000;
/// A rung on Claude Code's own ladder: its auto-compact fires at the
/// effective window less 13000, its warning 20000 below that. 80k remaining
/// is this nudge, 33k the CC warning, 13k the CC auto-compact.
const RESERVE: u64 = 80_000;
/// Flush cadence: more dirty files than this, static for this many
/// turn-ends. Conservative starting points until a distribution is sampled.
const FLUSH_FILE_THRESHOLD: u64 = 10;
const FLUSH_STATIC_STOPS: u64 = 3;
/// The probe cache window, seconds.
const PROBE_MAX_AGE: u64 = 300;
/// The operator-capture cache window: an ack must retire the nag promptly.
const OPCAP_MAX_AGE: u64 = 60;
/// A latch older than this many whole days is swept. Two days, not one: a
/// long session must not have its own latch swept mid-flight.
const LATCH_MAX_AGE_DAYS: u64 = 2;
/// The shell appender's one-line cap.
const EVENT_LINE_MAX_BYTES: usize = 4000;
/// The rooted-writer measurement and the git reads had no bound in the
/// shell; this keeps a hung child from eating the whole leg timeout.
const CHILD_SECS: u64 = 30;

fn context_nudge(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some(CACHE_REFRESH) {
        return cache_refresh(&args[1..]);
    }
    let (_raw, payload) = super::payload();
    // stop_hook_active: Stop re-fired after a block (mid-compaction, or
    // otherwise continuing). Re-blocking loops, so answer nothing.
    if payload.stop_hook_active == Some(true) {
        return 0;
    }
    let Some(transcript) = payload
        .transcript_path
        .clone()
        .filter(|t| !t.is_empty())
        .map(PathBuf::from)
        .filter(|t| t.is_file())
    else {
        return 0;
    };
    let mut session_id = payload.session().to_string();
    if session_id.is_empty() {
        session_id = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    }
    let tbase = transcript_base(&transcript);
    let cwd = super::cwd();
    let repo_root = super::repo_root(&cwd);
    let plugin_root = super::plugin_root();
    let budget = super::budget_secs();
    let fno = super::fno_bin();

    // Both triggers from one `fno config get target.handoff` boot.
    let (general_trigger, lead_trigger) = match (&fno, budget > 0) {
        (Some(fno), true) => {
            let mut cmd = Command::new(fno);
            cmd.args(["config", "get", "target.handoff"]);
            parse_triggers(&stdout_any(super::bounded(cmd, None, budget)))
        }
        _ => (50, 40),
    };

    // The probe rides the stale-while-revalidate cache, fingerprinted by a
    // cksum of the transcript bytes so any changed transcript re-measures.
    let fingerprint = cksum_fingerprint(&transcript);
    let probe_out = {
        let t = transcript.clone();
        let sid = session_id.clone();
        let here = cwd.clone();
        let live = move || probe_live(&t, &sid, &here);
        let refresh = super::fno_agent_bin()
            .map(|bin| {
                vec![
                    bin.to_string_lossy().into_owned(),
                    "context-run".to_string(),
                    "--probe".to_string(),
                    "--transcript".to_string(),
                    transcript.to_string_lossy().into_owned(),
                    "--session".to_string(),
                    session_id.clone(),
                    "--json".to_string(),
                ]
            })
            .unwrap_or_default();
        cache_serve(
            &format!("ctxprobe-{tbase}"),
            PROBE_MAX_AGE,
            &fingerprint,
            &live,
            &refresh,
        )
        .0
    };
    let probe = Probe::parse(&probe_out);

    // The registry, best-effort: an unreadable one reads as non-teamed.
    let agents = read_registry();
    let me = agents
        .as_deref()
        .and_then(|rows| my_row(rows, &session_id))
        .cloned();
    let team_level = me
        .as_ref()
        .map(|r| r.role_level.clone())
        .filter(level_present);
    let team_scope = me
        .as_ref()
        .and_then(|r| r.role_scope.clone())
        .unwrap_or_default();
    let lead_row_cwd = me.as_ref().and_then(|r| r.cwd.clone()).unwrap_or_default();
    let is_lead = team_level.is_some();
    let buckets = match (&agents, is_lead) {
        (Some(rows), true) => Buckets::of(rows, &session_id),
        _ => Buckets::default(),
    };
    let effective_trigger = if is_lead {
        lead_trigger
    } else {
        general_trigger
    };

    let band = probe.used_pct.map(|p| p / 10).unwrap_or(0);
    // Latches are per-session state keyed on the transcript basename, so
    // they live in the global state dir's state/latches, which follows an
    // overridden config.state_dir.
    let state_dir = state_dir(&cwd);
    let latch_dir = state_dir.join("state/latches");
    let _ = std::fs::create_dir_all(&latch_dir);
    sweep_latches(&latch_dir);
    let latches = Latches::new(&latch_dir, &tbase, band);
    let events = Events {
        repo_root: repo_root.clone(),
        state_dir: state_dir.clone(),
    };

    let mut reasons: Vec<String> = Vec::new();

    // Check (a): context pressure, every session.
    let (fire_ctx, preparation) = probe.fire(effective_trigger);
    if fire_ctx && !latches.ctx.exists() {
        touch(&latches.ctx);
        let codex = std::env::var("FNO_HARNESS").as_deref() == Ok("codex")
            || std::env::var_os("CODEX_THREAD_ID").is_some_and(|v| !v.is_empty());
        let compact_ask = if codex {
            if preparation {
                "Astra preparation band reached. Preserve volatile decisions and continue; the provider compact action waits for the action band.".to_string()
            } else {
                provider_text(&provider_compaction_receipt(&session_id, &cwd), &session_id)
            }
        } else {
            compact_text(&compact_check(fno.as_deref(), budget))
        };
        let pct = probe.used_pct.unwrap_or(0);
        if is_lead {
            events.emit(
                "lead_context_nudge",
                json!({"used_pct": pct, "trigger": lead_trigger, "role_level": team_level.clone().unwrap_or(Value::Null), "role_scope": team_scope, "session_id": session_id}),
            );
            let rows = agents.as_deref().unwrap_or(&[]);
            let canon_doc = fno
                .as_deref()
                .map(|fno| {
                    let mut cmd = Command::new(fno);
                    cmd.args(["config", "paths", "handoff"]);
                    if team_scope.is_empty() {
                        cmd.args(["--session-id", &session_id]);
                    } else {
                        cmd.args(["--scope", &team_scope]);
                    }
                    first_line(&stdout_any(super::bounded(cmd, None, budget)))
                })
                .unwrap_or_default();
            reasons.push(lead_reason(
                &probe,
                &team_scope,
                &compact_ask,
                &canon_doc,
                preparation,
                &rollup(rows, &team_scope),
            ));
        } else {
            events.emit(
                "session_context_nudge",
                json!({"used_pct": pct, "trigger": general_trigger, "session_id": session_id}),
            );
            // plan_path only words the ask; it gates nothing.
            let plan_path = super::fno_agent_bin()
                .map(|bin| {
                    let mut cmd = Command::new(bin);
                    cmd.args([
                        "state-cli",
                        "show",
                        "--type",
                        "target",
                        "--field",
                        "plan_path",
                    ]);
                    first_line(&stdout_any(super::bounded(cmd, None, budget)))
                })
                .unwrap_or_default();
            let canon_doc = if plan_path.is_empty() {
                fno.as_deref()
                    .map(|fno| {
                        let mut cmd = Command::new(fno);
                        cmd.args(["config", "paths", "handoff", "--session-id", &session_id]);
                        first_line(&stdout_any(super::bounded(cmd, None, budget)))
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            reasons.push(session_reason(
                &probe,
                general_trigger,
                &compact_ask,
                &plan_path,
                &canon_doc,
                preparation,
            ));
        }
    }

    // Check (b): orphaned live children, teamed leads only, own latch.
    if is_lead && buckets.any() && !latches.orphan.exists() {
        let mut resolved = false;
        // Resolution 1: the holder declared the team on the manifest, read
        // from the ROLE ROW's cwd space, never this leg's cwd.
        if !team_scope.is_empty() {
            if let Some(manifest) =
                lead_manifest_path_for_row(&lead_row_cwd, &team_scope, &cwd, &state_dir)
            {
                if let Ok(text) = std::fs::read_to_string(&manifest) {
                    resolved = shape_declares_team(&text);
                }
            }
        }
        // Resolution 3: a recent carveout carrying THIS scope, filed by any
        // session (`--all`). A broken reader never clears the guard.
        if let Some(fno) = fno.as_deref() {
            let mut cmd = Command::new(fno);
            cmd.args(["backlog", "carveout", "list", "--all", "--json"]);
            let ledger = stdout_any(super::bounded(cmd, None, budget));
            let cutoff = utc_stamp(SystemTime::now() - Duration::from_secs(24 * 3600));
            if carveout_hit(&ledger, &team_scope, &cutoff) {
                resolved = true;
            }
        }
        if !resolved {
            touch(&latches.orphan);
            events.emit(
                "lead_orphan_block",
                json!({
                    "role_level": team_level.clone().unwrap_or(Value::Null),
                    "role_scope": team_scope,
                    "workers": buckets.linked.names(),
                    "count": buckets.linked.count(),
                    "unknown_workers": buckets.unknown.names(),
                    "unknown_count": buckets.unknown.count(),
                    "unlinked_workers": buckets.unlinked.names(),
                    "unlinked_count": buckets.unlinked.count(),
                    "unlinked_unknown_workers": buckets.unlinked_unknown.names(),
                    "unlinked_unknown_count": buckets.unlinked_unknown.count(),
                    "session_id": session_id,
                }),
            );
            reasons.push(orphan_reason(&team_scope, &buckets));
        }
    }

    // Check (c): flush cadence, work only in volatile state. git is the
    // code-vertical detector; no repo degrades to silence.
    let dirty = git_out(&repo_root, &["status", "--porcelain"])
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    let head = git_out(&repo_root, &["rev-parse", "HEAD"])
        .trim_end_matches('\n')
        .to_string();
    let mut fire_flush = false;
    let mut static_stops = 0;
    if !head.is_empty() {
        let stored = std::fs::read_to_string(&latches.flush_state).unwrap_or_default();
        static_stops = next_static_count(stored.trim_end_matches('\n'), &head);
        let _ = std::fs::write(&latches.flush_state, format!("{head} {static_stops}\n"));
        fire_flush = dirty > FLUSH_FILE_THRESHOLD
            && static_stops >= FLUSH_STATIC_STOPS
            && !latches.flush.exists();
    }
    if fire_flush {
        // Whose dirt is this? Unmeasurable fails closed, never reads empty.
        let root_text = repo_root.to_string_lossy().into_owned();
        match foreign_rooted_writers(&plugin_root, &repo_root, &session_id, &state_dir) {
            Err(()) => {
                events.emit(
                    "context_flush_refused",
                    json!({"dirty_files": dirty, "readable": false, "writers": "", "session_id": session_id}),
                );
                reasons.push(format!(
                    "flush: REFUSED. {dirty} files are dirty in {root_text}, but whether a foreign live writer is rooted there could not be measured (process snapshot or agents registry unreadable), so the dirt cannot be read as yours alone. Do NOT run 'git add -A' or 'git commit -a' - stage only your own paths by name, and leave the rest."
                ));
            }
            Ok(foreign) if !foreign.is_empty() => {
                // Refused once already: stay silent. The flush latch stays
                // unconsumed so the real nudge fires once the writer leaves.
                if !latches.foreign.exists() {
                    touch(&latches.foreign);
                    events.emit(
                        "context_flush_refused",
                        json!({"dirty_files": dirty, "readable": true, "writers": foreign.replace('\t', ":").replace('\n', ","), "session_id": session_id}),
                    );
                    reasons.push(foreign_reason(dirty, &root_text, &foreign));
                }
            }
            Ok(_) => {
                touch(&latches.flush);
                events.emit(
                    "context_flush_nudge",
                    json!({"dirty_files": dirty, "static_stops": static_stops, "session_id": session_id}),
                );
                reasons.push(format!(
                    "flush: {dirty} files have uncommitted changes and the tree has not moved in {static_stops} turn-ends, so this work exists only in this session's volatile state - a compact or a crashed pane loses it. Fix what you found if it is small: commit it now, in this PR, as its own atomic commit. File a carveout ONLY for what is genuinely separable: 'fno backlog carveout add -k deferred \"...\"'. A session that never reaches the end of a unit of work records nothing."
                ));
            }
        }
    }

    if !reasons.is_empty() {
        super::emit_block(&reasons.join("  ||  "));
    }
    0
}

/// `basename <transcript> .jsonl`.
fn transcript_base(transcript: &Path) -> String {
    let name = transcript
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.strip_suffix(".jsonl") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => name,
    }
}

/// The two triggers from the `target.handoff` block; an unreadable or
/// non-numeric value keeps its default (50 general, 40 lead).
fn parse_triggers(block: &str) -> (u64, u64) {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Handoff {
        used_pct_trigger: Option<Value>,
        lead_used_pct_trigger: Option<Value>,
    }
    let h: Handoff = serde_json::from_str(block.trim()).unwrap_or_default();
    let num = |v: Option<Value>, d: u64| v.and_then(|v| v.as_u64()).unwrap_or(d);
    (
        num(h.used_pct_trigger, 50),
        num(h.lead_used_pct_trigger, 40),
    )
}

/// The probe's answer: `used_pct` must read as a whole number or the
/// reading is absent (no pressure).
#[derive(Default, Debug)]
struct Probe {
    used_pct: Option<u64>,
    used_tokens: Option<u64>,
    window_tokens: Option<u64>,
    band: String,
}

impl Probe {
    fn parse(out: &str) -> Self {
        // Raw values: a field of the wrong type reads as absent, not as a
        // parse failure of the whole answer.
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Answer {
            used_pct: Value,
            used_tokens: Value,
            window_tokens: Value,
            compaction_band: Value,
        }
        let Some(v) = serde_json::from_str::<Value>(out.trim())
            .ok()
            .filter(Value::is_object)
            .and_then(|v| Answer::deserialize(v).ok())
        else {
            return Probe::default();
        };
        let Some(pct) = v.used_pct.as_u64() else {
            return Probe::default();
        };
        Probe {
            used_pct: Some(pct),
            used_tokens: v.used_tokens.as_u64(),
            window_tokens: v.window_tokens.as_u64(),
            band: v.compaction_band.as_str().unwrap_or("").to_string(),
        }
    }

    /// (fire, preparation). Quality branch: a large window past the
    /// trigger. Capacity branch: any window at the absolute reserve. An
    /// Astra band (prepare or action) keeps the measured path alive.
    fn fire(&self, trigger: u64) -> (bool, bool) {
        let (Some(pct), Some(window)) = (self.used_pct, self.window_tokens) else {
            return (false, false);
        };
        let mut fire = window >= MIN_WINDOW && pct >= trigger;
        if let Some(used) = self.used_tokens {
            if (window as i128) - (used as i128) <= RESERVE as i128 {
                fire = true;
            }
        }
        if self.band == "prepare" || self.band == "action" {
            fire = true;
        }
        (fire, self.band == "prepare")
    }

    fn tokens(v: Option<u64>) -> String {
        v.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string())
    }
}

/// The in-process probe (`fno-agent context-run --probe --json`) under the
/// load-aware budget. A read past the budget reads as silence; a failed
/// read carries the CLI's exit 3 with no output.
fn probe_live(transcript: &Path, session: &str, cwd: &Path) -> (String, i32) {
    let budget = super::budget_secs();
    if budget == 0 {
        return (String::new(), 0);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (t, s, c) = (
        transcript.to_path_buf(),
        session.to_string(),
        cwd.to_path_buf(),
    );
    let spawned = std::thread::Builder::new()
        .name("context-nudge-probe".into())
        .spawn(move || {
            let _ = tx.send(crate::context_run::transcript_reading(&t, &s, &c));
        });
    if spawned.is_err() {
        return (String::new(), 0);
    }
    match rx.recv_timeout(Duration::from_secs(budget)) {
        Ok(Ok(reading)) => (reading.to_string(), 0),
        Ok(Err(_)) => (String::new(), 3),
        Err(_) => (String::new(), 0),
    }
}

/// `fno agent internal registry-json`, read in process.
fn read_registry() -> Option<Vec<AgentRow>> {
    let home = crate::paths::AgentsHome::from_env();
    let rows = crate::client_commands::load_registry_entries(&home.registry_json()).ok()?;
    let projected = crate::registry_json::registry_json_logic(&rows);
    #[derive(Deserialize)]
    struct Projected {
        agents: Vec<Value>,
    }
    let projected = Some(&projected)
        .filter(|v| v.is_object())
        .and_then(|v| Projected::deserialize(v).ok())?;
    Some(
        projected
            .agents
            .into_iter()
            .map(|row| serde_json::from_value(row).unwrap_or_default())
            .collect(),
    )
}

/// One served registry row, the fields the checks read.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default)]
struct AgentRow {
    name: Option<String>,
    session_id: Option<String>,
    harness_session_id: Option<String>,
    role_level: Value,
    role_scope: Option<String>,
    cwd: Option<String>,
    spawned_by_session: Option<String>,
    origin: Option<String>,
    liveness: Option<String>,
    spawn_provenance: Value,
}

fn my_row<'a>(rows: &'a [AgentRow], sid: &str) -> Option<&'a AgentRow> {
    rows.iter().find(|r| {
        r.session_id.as_deref() == Some(sid) || r.harness_session_id.as_deref() == Some(sid)
    })
}

/// jq `.role_level // empty` read non-empty: teamed.
fn level_present(level: &Value) -> bool {
    match level {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// jq `(.role_level // 0) == 0`.
fn level_is_zero(level: &Value) -> bool {
    match level {
        Value::Null | Value::Bool(false) => true,
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// jq `(.role_level // 0) > 0`: strings, arrays and objects order above
/// every number.
fn level_positive(level: &Value) -> bool {
    match level {
        Value::Number(n) => n.as_f64().is_some_and(|f| f > 0.0),
        Value::String(_) | Value::Array(_) | Value::Object(_) => true,
        _ => false,
    }
}

#[derive(Default, Debug)]
struct Bucket(Vec<String>);

impl Bucket {
    fn names(&self) -> String {
        self.0.join(", ")
    }
    fn count(&self) -> usize {
        self.0.len()
    }
    fn or_none(&self) -> String {
        if self.0.is_empty() {
            "none".to_string()
        } else {
            self.names()
        }
    }
}

/// The four worker buckets of the orphan check. The served liveness word
/// decides: a null or stale word is unresolved, never alive and never
/// dropped, so a broken reader can never clear the guard by going quiet.
#[derive(Default, Debug)]
struct Buckets {
    linked: Bucket,
    unknown: Bucket,
    unlinked: Bucket,
    unlinked_unknown: Bucket,
}

impl Buckets {
    fn of(rows: &[AgentRow], sid: &str) -> Self {
        let mut b = Buckets::default();
        for r in rows {
            let name = r.name.clone().unwrap_or_default();
            let alive = r.liveness.as_deref() == Some("alive");
            let resolved = alive || r.liveness.as_deref() == Some("dead");
            if r.spawned_by_session.as_deref() == Some(sid) {
                if alive {
                    b.linked.0.push(name.clone());
                } else if !resolved {
                    b.unknown.0.push(name.clone());
                }
            }
            // A door-stamped row names its owner: owned autonomous work,
            // never an unlinked orphan.
            let own = r
                .session_id
                .as_deref()
                .or(r.harness_session_id.as_deref())
                .unwrap_or("");
            let unlinked = r.spawned_by_session.as_deref().unwrap_or("").is_empty()
                && r.origin.as_deref() != Some("operator")
                && level_is_zero(&r.role_level)
                && own != sid
                && r.spawn_provenance.is_null();
            if unlinked {
                if alive {
                    b.unlinked.0.push(name);
                } else if !resolved {
                    b.unlinked_unknown.0.push(name);
                }
            }
        }
        b
    }

    fn any(&self) -> bool {
        self.linked.count()
            + self.unknown.count()
            + self.unlinked.count()
            + self.unlinked_unknown.count()
            > 0
    }
}

/// The lead's neighbourhood from the same registry read: peer leads over a
/// different scope, and a lead above whose scope prefixes this one.
fn rollup(rows: &[AgentRow], scope: &str) -> String {
    let peers: Vec<&AgentRow> = rows
        .iter()
        .filter(|r| level_positive(&r.role_level) && r.role_scope.as_deref().unwrap_or("") != scope)
        .collect();
    let above = peers
        .iter()
        .filter(|r| scope.starts_with(r.role_scope.as_deref().unwrap_or("")))
        .count();
    let mut out = String::new();
    if !peers.is_empty() {
        out = format!(" {} peer lead(s) also in flight.", peers.len());
    }
    if above > 0 {
        out.push_str(" A lead above holds your scope.");
    }
    out
}

/// The lead manifest path from the role row, computed in process: the file
/// lives at <spaces root>/<slug of the canonical main checkout root of the
/// row's cwd>/leads/<scope>.md. A layout where the root reads differently
/// from the writer reads as no manifest: the nag survives.
fn lead_manifest_path_for_row(
    row_cwd: &str,
    scope: &str,
    here: &Path,
    state_dir: &Path,
) -> Option<PathBuf> {
    if scope.contains('/') || scope.contains('\\') || scope.contains("..") {
        return None;
    }
    let d = if row_cwd.is_empty() {
        here.to_path_buf()
    } else {
        PathBuf::from(row_cwd)
    };
    let root = canonical_root(&d)?;
    let spaces = if let Some(v) = env_nonempty("FNO_SPACES_DIR") {
        PathBuf::from(v)
    } else if let Some(v) = env_nonempty("FNO_STATE_DIR") {
        PathBuf::from(v).join("spaces")
    } else {
        state_dir.join("spaces")
    };
    let slug = root.to_string_lossy().replace('/', "-");
    Some(spaces.join(slug).join("leads").join(format!("{scope}.md")))
}

/// Marker-first canonical root: a checkout's own .git dir, a linked
/// worktree's commondir chain, git worktree list's first row, then the
/// canonicalized dir itself.
fn canonical_root(d: &Path) -> Option<PathBuf> {
    let dot_git = d.join(".git");
    if dot_git.is_dir() {
        if let Ok(root) = std::fs::canonicalize(d) {
            return Some(root);
        }
    } else if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).unwrap_or_default();
        let gitdir = text
            .lines()
            .find_map(|l| l.strip_prefix("gitdir:"))
            .map(|g| g.trim_start().to_string())
            .filter(|g| !g.is_empty());
        if let Some(gitdir) = gitdir {
            let gitdir = if gitdir.starts_with('/') {
                PathBuf::from(gitdir)
            } else {
                d.join(gitdir)
            };
            let common = std::fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .and_then(|c| c.lines().next().map(str::to_string))
                .filter(|c| !c.is_empty());
            if let Some(common) = common {
                let common = if common.starts_with('/') {
                    PathBuf::from(common)
                } else {
                    gitdir.join(common)
                };
                if let Ok(common) = std::fs::canonicalize(common) {
                    if common.file_name().is_some_and(|n| n == ".git") && common.is_dir() {
                        if let Some(root) =
                            common.parent().and_then(|p| std::fs::canonicalize(p).ok())
                        {
                            return Some(root);
                        }
                    }
                }
            }
        }
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(d)
        .args(["worktree", "list", "--porcelain"]);
    let listed = stdout_any(super::bounded(cmd, None, 3));
    if let Some(first) = listed
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("worktree "))
    {
        if let Ok(root) = std::fs::canonicalize(first) {
            return Some(root);
        }
    }
    std::fs::canonicalize(d).ok()
}

/// A manifest whose first `shape:` line reads org or team: the holder
/// declared the team, and the orphan nag is answered.
fn shape_declares_team(manifest: &str) -> bool {
    let shape: String = manifest
        .lines()
        .find_map(|l| l.strip_prefix("shape:"))
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    shape == "org" || shape == "team"
}

/// One carveout row (JSONL) carrying `scope` and filed at or after
/// `cutoff`. ISO-8601 UTC strings compare lexicographically.
fn carveout_hit(ledger: &str, scope: &str, cutoff: &str) -> bool {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Carveout {
        scope: Option<String>,
        ts: Option<Value>,
    }
    ledger.lines().any(|line| {
        let Ok(row) = serde_json::from_str::<Carveout>(line.trim()) else {
            return false;
        };
        let recent = match &row.ts {
            None | Some(Value::Null) => "0" >= cutoff,
            Some(Value::String(ts)) => ts.as_str() >= cutoff,
            Some(_) => false,
        };
        row.scope.as_deref() == Some(scope) && recent
    })
}

/// `fno agent mail send '/compact' --to-self --raw --check`: the one
/// resolver answering about itself, asked with --to-self because the
/// verdict differs by recipient. A floor of 4 s, not the scaled budget: it
/// fires at most once per band and is the gate's one measured answer.
fn compact_check(fno: Option<&Path>, budget: u64) -> String {
    let Some(fno) = fno else {
        return String::new();
    };
    let mut cmd = Command::new(fno);
    cmd.args([
        "agent",
        "mail",
        "send",
        "/compact",
        "--to-self",
        "--raw",
        "--check",
    ]);
    stdout_any(super::bounded(cmd, None, budget.max(4)))
}

/// THREE answers, chosen by the verdict word the check printed: "I
/// measured no path" and "I could not measure" are different claims.
fn compact_text(out: &str) -> String {
    let ask = "ask your operator to type /compact <brief-path> at your prompt, and say in one line what to preserve";
    if out.starts_with("injectable:") {
        format!(
            "/compact is a REPL built-in your Skill tool cannot call, and this session HAS an injection path (measured just now: {out}), so fire it at your own prompt line: fno agent mail send '/compact <brief-path>' --to-self --raw   (the leading slash is load-bearing, and the brief path matters: a bare /compact at exhausted context has no headroom left to summarize). A path is not a landing - if the receipt comes back unconfirmed, read your own prompt before assuming either way, and never re-send."
        )
    } else if out.starts_with("not-injectable:") {
        format!(
            "/compact is a REPL built-in your Skill tool cannot call, and this session has NO injection path: 'fno agent mail send /compact --to-self --raw --check' answered {out}. That means no registry row, a non-keystroke lane, a guarded mux pane (which refuses a mid-turn recipient, and you are mid-turn), or no control socket to write into. It is NOT a claim that you are dead. You cannot fire the command yourself, so {ask}. On whether /fno-me would help: it adds the registry row this check needs FIRST, and nothing else - it creates no daemon roster entry and no control socket - so it flips this answer only for a session that already had those and merely lacked a row. Read the reason above rather than guessing which case you are."
        )
    } else {
        format!(
            "/compact is a REPL built-in your Skill tool cannot call, and whether you can inject it at your own prompt line could not be measured here (no fno on PATH, or the check timed out), so this nudge will not claim you have a path. Either {ask}, or check for yourself with 'fno agent mail send /compact --to-self --raw --check' and fire it if that says injectable."
        )
    }
}

/// The codex lane asks the provider to compact the exact thread, bounded
/// at 45 s: it is a decision, not a read.
fn provider_compaction_receipt(session: &str, cwd: &Path) -> String {
    let Some(bin) = super::fno_agent_bin().filter(|_| !session.is_empty()) else {
        return String::new();
    };
    let mut cmd = Command::new(bin);
    cmd.args(["loop", "command", "--session", session, "--cwd"])
        .arg(cwd)
        .args(["--method", "thread/compact/start", "--text", "/compact"]);
    stdout_any(super::bounded(cmd, None, 45))
}

fn provider_text(out: &str, session: &str) -> String {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Receipt {
        verified: Option<bool>,
        action: Option<String>,
        status: Option<String>,
        thread_id: Option<String>,
    }
    let verified = !out.is_empty()
        && serde_json::from_str::<Receipt>(out.trim()).is_ok_and(|r| {
            r.verified == Some(true)
                && r.action.as_deref() == Some("compact")
                && r.status.as_deref() == Some("completed")
                && r.thread_id.as_deref() == Some(session)
        });
    if verified {
        format!("Codex provider compaction completed for exact thread {session}; verified receipt: {out}")
    } else {
        let shown = if session.is_empty() {
            "unknown"
        } else {
            session
        };
        format!("Codex provider compaction was not verified for exact thread {shown}. Do not retry this action blindly; ask the operator to inspect provider state.")
    }
}

/// The teamed-lead context reason. A team survives a compact, so the
/// percentage is a compact trigger; handoff is a judgement about ruling
/// quality and carries no number. The stored rung is not printed: it is
/// stale for every lead stamped before succession moved into spawn.
fn lead_reason(
    probe: &Probe,
    scope: &str,
    compact_ask: &str,
    canon_doc: &str,
    preparation: bool,
    rollup: &str,
) -> String {
    let pct = probe.used_pct.unwrap_or(0);
    let used = Probe::tokens(probe.used_tokens);
    let window = Probe::tokens(probe.window_tokens);
    // On a first compaction the canon doc does not exist yet, so the ask
    // names the exact path and the literal headings: the PreCompact hook
    // binds each block by its heading text, not by file position.
    let doc_ask = if canon_doc.is_empty() {
        "fill its two team-only headings yourself - gaps and open thinking, and workarounds in force - since nothing else knows what only you hold.".to_string()
    } else {
        format!(
            "write (or append) this into {canon_doc} yourself before you compact, even if that file does not exist yet - the hook binds each block by its heading text, not by file position, so this survives regardless: '## Gaps and open thinking (session)' then a line '<!-- fno:session -->', then your own text, then '<!-- /fno:session -->'; repeat the same shape with heading '## Workarounds in force (session)'. Nothing else knows what only you hold."
        )
    };
    let action = if preparation {
        "PREPARE TO COMPACT AND KEEP RULING; do not compact until the action band"
    } else {
        "COMPACT AND KEEP RULING"
    };
    format!(
        "context: {pct}% used ({used} of {window} tokens). You hold the team over {scope}. A team is maintained across a compact - your team, session id, mail handle, and claims all come out the other side - so the move here is to {action}. {compact_ask} The PreCompact hook writes your team, scope, nodes under purview, and live workers into the canon doc automatically; before you compact, {doc_ask} Handing off is a different decision and this percentage is not its trigger: hand off when your ORCHESTRATION is visibly degrading (you are making worse calls, losing threads, repeating yourself) and a fresh session would rule {scope} better. Ask yourself that about your last few rulings, not about this number. The cost is concrete either way: a successor gets a NEW mail handle, so every worker still holding yours is orphaned at review. If you judge a handoff is right anyway: bash skills/target/scripts/handoff.sh, or spawn your successor over your own scope, which transfers the team in the same atomic write that vacates yours - 'fno agent spawn -k \"{scope}\" \"<seed prompt>\"' - and close this pane only after the successor's session header prints.{rollup}"
    )
}

/// The every-session context reason, shaped by what already survives a
/// compact: a bound plan, or nothing at all.
fn session_reason(
    probe: &Probe,
    trigger: u64,
    compact_ask: &str,
    plan_path: &str,
    canon_doc: &str,
    preparation: bool,
) -> String {
    let pct = probe.used_pct.unwrap_or(0);
    let used = Probe::tokens(probe.used_tokens);
    let window = Probe::tokens(probe.window_tokens);
    let core = if preparation {
        format!(
            "context: {pct}% used ({used} of {window} tokens). The Astra preparation band is active. Preserve volatile decisions and continue; the provider compact action waits for the action band. {compact_ask}"
        )
    } else {
        format!(
            "context: {pct}% used ({used} of {window} tokens). You are past the session compact trigger ({trigger}%). Returns diminish well before this window fills, so a long run degrades from here. Compact now: your session id, mail handle, and claims all survive it. {compact_ask}"
        )
    };
    if !plan_path.is_empty() {
        format!(
            "{core} You have a plan bound, so the plan, STATE.md and SUMMARY.md survive the compact - but your in-flight judgment does not. Before you compact, flush what is only in volatile state: commit small fixes in this PR as their own atomic commit, 'fno backlog carveout add -k deferred \"...\"' for separable work, 'fno backlog idea --audience internal --why ... --done-means ...' for new findings, and note any plan drift in SUMMARY.md."
        )
    } else if !canon_doc.is_empty() {
        format!(
            "{core} You have no plan and no team, so nothing about this session's work survives a compact unless you write it down. Before you compact, write a brief canon doc at {canon_doc} - a markdown file with what you are doing, the key decisions, and the open threads - and commit it, so a fresh session or a successor can pick up where you left off. The PreCompact hook keeps that doc's mechanical sections fresh; you fill its merge-order and open-decisions sections."
        )
    } else {
        format!(
            "{core} You have no plan and no team, so nothing about this session's work survives a compact unless you write it down. Before you compact, write a brief canon doc - a markdown file with what you are doing, the key decisions, and the open threads - and commit it, so a fresh session or a successor can pick up where you left off."
        )
    }
}

fn orphan_reason(scope: &str, b: &Buckets) -> String {
    format!(
        "You hold the team over {scope}. The served liveness word from 'fno agent internal registry-json' says {} worker(s) you spawned are still alive ({}). Linked count: {}. {} spawned worker row(s) have unresolved liveness ({}); a broken reader never clears this guard, so they count on their own and stay out of the linked obligation above. {} active worker row(s) have no spawned_by link ({}); ownership unknown, so they cannot be excluded from this team's obligations. {} unlinked worker row(s) also have unresolved liveness ({}); same reason, they count on their own. A lead that spawns workers cannot be a pure pass: stepping down now leaves them with nobody to mail when they reach review. Pick one and act, then this stops: (1) stay as team through the wave with 'fno agent hq chart shape team'; (2) hand the team to a successor by spawning it over your own scope, which vacates yours in the same atomic write - 'fno agent spawn -k \"{scope}\" \"<seed prompt>\"'; (3) record that these workers are review-orphaned with 'fno backlog carveout add -k deferred --scope {scope} \"...\"' and they fall back to advisory self-review. Check 'fno agent internal registry-json' for spawned_by_session null to close the ownership gap.",
        b.linked.count(),
        b.linked.or_none(),
        b.linked.count(),
        b.unknown.count(),
        b.unknown.or_none(),
        b.unlinked.count(),
        b.unlinked.or_none(),
        b.unlinked_unknown.count(),
        b.unlinked_unknown.or_none(),
    )
}

/// The refusal naming the first foreign writer (`pid\tname\tsid` lines).
fn foreign_reason(dirty: u64, root: &str, foreign: &str) -> String {
    let rows: Vec<&str> = foreign.lines().filter(|l| !l.is_empty()).collect();
    let mut first = rows.first().copied().unwrap_or("").split('\t');
    let pid = first.next().unwrap_or("");
    let name = first.next().unwrap_or("");
    let sid = first.next().unwrap_or("");
    let more = if rows.len() > 1 {
        format!(" (and {} more rooted writer(s))", rows.len() - 1)
    } else {
        String::new()
    };
    format!(
        "flush: REFUSED. {dirty} files are dirty in {root}, but a live writer that is not this session is rooted there: {name} (pid {pid}, session {sid}){more}. This diff is not yours to commit. Do NOT run 'git add -A' or 'git commit -a' here - it would put another session's mid-flight work on this branch, unreviewed, attributed to you. If some of the dirt IS yours, stage only your own paths by name. Otherwise leave it: it is reported, not orphaned."
    )
}

/// Consecutive Stops that saw the same HEAD, from the stored `<head> <n>`.
fn next_static_count(stored: &str, head: &str) -> u64 {
    let Some((stored_head, count)) = stored.split_once(' ') else {
        return 1;
    };
    if stored_head != head {
        return 1;
    }
    let count = if !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()) {
        count.parse::<u64>().unwrap_or(0)
    } else {
        0
    };
    count + 1
}

/// scripts/lib/rooted-writers.sh's `foreign_rooted_writers`, run as a
/// bounded bash child: it reuses the worktree-lifecycle process lane, which
/// has no Rust port. Ok holds its `pid\tname\tsid` lines (empty means none);
/// Err means the measurement could not be made and the caller fails closed.
fn foreign_rooted_writers(
    plugin_root: &Path,
    repo_root: &Path,
    sid: &str,
    state_dir: &Path,
) -> Result<String, ()> {
    let lib = plugin_root.join("scripts/lib/rooted-writers.sh");
    if !lib.is_file() {
        return Err(());
    }
    let path = format!(
        "{}{}/usr/bin:/bin:/usr/sbin:/sbin",
        std::env::var("PATH").unwrap_or_default(),
        if std::env::var("PATH").is_ok_and(|p| !p.is_empty()) {
            ":"
        } else {
            ""
        }
    );
    let mut cmd = Command::new("/bin/bash");
    cmd.arg("-c")
        .arg(r#"source "$1" 2>/dev/null; declare -F foreign_rooted_writers >/dev/null 2>&1 || exit 2; foreign_rooted_writers "$2" "$3""#)
        .arg("bash")
        .arg(&lib)
        .arg(repo_root)
        .arg(sid)
        .env("PATH", path)
        .env("STATE_DIR", state_dir);
    match super::bounded(cmd, None, CHILD_SECS) {
        Some(out) if out.status.code() == Some(0) => Ok(String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string()),
        _ => Err(()),
    }
}

/// The latch files for one transcript and band, byte-for-byte the shell
/// names so old and new sessions share one state.
struct Latches {
    ctx: PathBuf,
    orphan: PathBuf,
    flush: PathBuf,
    /// The foreign-writer refusal latches apart from the flush latch, so a
    /// refusal never silences the real nudge.
    foreign: PathBuf,
    /// Static-HEAD tracking is band-independent.
    flush_state: PathBuf,
}

impl Latches {
    fn new(dir: &Path, tbase: &str, band: u64) -> Self {
        Latches {
            ctx: dir.join(format!(".context-nudge-ctx-{tbase}-{band}")),
            orphan: dir.join(format!(".context-nudge-orphan-{tbase}-{band}")),
            flush: dir.join(format!(".context-nudge-flush-latch-{tbase}-{band}")),
            foreign: dir.join(format!(".context-nudge-flush-foreign-{tbase}-{band}")),
            flush_state: dir.join(format!(".context-nudge-flush-{tbase}")),
        }
    }
}

/// The writer owns the lifetime: a latch is keyed to one session, so it is
/// swept once it is past LATCH_MAX_AGE_DAYS whole days old.
fn sweep_latches(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if age / 86_400 > LATCH_MAX_AGE_DAYS {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The live state root (`fno config paths shell-stub`'s STATE_DIR), or
/// `$HOME/.fno` when it cannot be resolved.
fn state_dir(cwd: &Path) -> PathBuf {
    crate::paths_cli::state_dir_absolute(cwd)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".fno"))
}

/// The two journals every nudge row lands in: the project's and the
/// global one.
struct Events {
    repo_root: PathBuf,
    state_dir: PathBuf,
}

impl Events {
    fn emit(&self, etype: &str, data: Value) {
        let line = json!({"ts": utc_stamp(SystemTime::now()), "type": etype, "source": "hook", "data": data});
        let global = env_nonempty("GLOBAL_EVENTS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.state_dir.join("events.jsonl"));
        append_bounded_event(&self.repo_root.join(".fno/events.jsonl"), &line);
        append_bounded_event(&global, &line);
    }
}

/// events.sh's `_append_bounded_event`: the line cap, then the
/// opt-in rule (an append never creates a project `.fno/`), then one store
/// commit.
fn append_bounded_event(path: &Path, line: &Value) {
    let text = line.to_string();
    if text.len() + 1 > EVENT_LINE_MAX_BYTES {
        eprintln!(
            "context_nudge: serialized event is {} bytes and exceeds the {EVENT_LINE_MAX_BYTES}-byte append cap",
            text.len() + 1
        );
        return;
    }
    if !events_may_create_parent(path) {
        return;
    }
    if let Err(e) = crate::claims::append_event_line(path, line, Duration::from_secs(2)) {
        eprintln!(
            "context_nudge: the event store refused the append to {}: {e}",
            path.display()
        );
    }
}

fn events_may_create_parent(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return true;
    };
    if parent.is_dir() {
        return true;
    }
    let state_root = env_nonempty("FNO_HOME").unwrap_or_else(|| {
        format!(
            "{}/.fno",
            std::env::var("HOME")
                .unwrap_or_default()
                .trim_end_matches('/')
        )
    });
    let state_root = PathBuf::from(state_root.trim_end_matches('/'));
    if parent == state_root || physical(parent) == physical(&state_root) {
        return true;
    }
    parent.file_name().is_none_or(|n| n != ".fno")
}

/// The deepest existing ancestor, canonicalized, with the missing tail.
fn physical(path: &Path) -> PathBuf {
    let mut dir = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !dir.is_dir() {
        match (dir.file_name().map(|n| n.to_os_string()), dir.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                dir = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = std::fs::canonicalize(&dir).unwrap_or(dir);
    for name in tail.iter().rev() {
        out.push(name);
    }
    out
}

fn operator_capture_nudge(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some(CACHE_REFRESH) {
        return cache_refresh(&args[1..]);
    }
    let Some(fno) = super::fno_bin() else {
        return 0;
    };
    // Past the overload threshold the preamble alone can pass the harness
    // cap; the depth reports next turn.
    if super::overloaded() {
        return 0;
    }
    let (_raw, payload) = super::payload();
    let session = payload.session().to_string();
    // A worker session reads as no queue.
    if std::env::var("FNO_SESSION_ROLE").as_deref() == Ok("worker") {
        return 0;
    }
    if !session.is_empty() {
        let role = crate::owner_ladder::role_now(
            &session,
            &crate::paths::AgentsHome::from_env(),
            &super::cwd(),
        );
        if role.word() == "worker" {
            return 0;
        }
    }
    // The inbox family lives at the Rust front door: the checkout's front
    // build first (debug outranks release), then the installed one.
    let root = super::plugin_root();
    let front = [
        "crates/fno/target/debug/fno",
        "crates/fno/target/release/fno",
    ]
    .iter()
    .map(|p| root.join(p))
    .find(|p| super::is_executable(p))
    .unwrap_or(fno);
    let argv: Vec<String> = vec![
        front.to_string_lossy().into_owned(),
        "inbox".into(),
        "operator".into(),
        "status".into(),
        "--json".into(),
    ];
    let live = || run_optional(&argv);
    let (out, rc) = if session.is_empty() {
        live()
    } else {
        cache_serve(&format!("opcap-{session}"), OPCAP_MAX_AGE, "", &live, &argv)
    };
    // A non-zero exit is not silence, but exit 2 is "no such command" from
    // a deployed fno older than this feature: nagging forever is noise.
    if rc != 0 {
        if rc != 2 {
            crate::hook::emit_line(&format!(
                "## Operator capture\n\ncould not be read (fno inbox operator status exit {rc}). Run it directly."
            ));
        }
        return 0;
    }
    if let Some(text) = operator_capture_text(&out) {
        crate::hook::emit_line(&text);
    }
    0
}

/// The capture report from the status payload. It takes the first object
/// that LOOKS LIKE this payload (carries `depth`), not the first that
/// parses: the command shares stdout with any preamble a deployed fno
/// prints, and a preamble can carry a brace. Offsets walk codepoints.
fn operator_capture_text(payload: &str) -> Option<String> {
    let c: Vec<char> = payload.chars().collect();
    let closes: Vec<usize> = (0..c.len()).filter(|&j| c[j] == '}').collect();
    let mut objs: Vec<serde_json::Map<String, Value>> = Vec::new();
    for i in (0..c.len()).filter(|&i| c[i] == '{') {
        let found = closes.iter().filter(|&&j| j > i).find_map(|&j| {
            let s: String = c[i..=j].iter().collect();
            serde_json::from_str::<Value>(&s)
                .ok()
                .filter(|v| !v.is_null())
        });
        if let Some(Value::Object(map)) = found {
            objs.push(map);
        }
    }
    let Some(p) = objs.iter().find(|o| o.contains_key("depth")) else {
        return (!objs.is_empty()).then(|| {
            "## Operator capture\n\nread returned a JSON object with no depth key; run `fno inbox operator status` directly.".to_string()
        });
    };
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Capture {
        depth: Value,
        oldest_age_s: Value,
        oldest_excerpt: Value,
        skipped: Value,
    }
    let p = Capture::deserialize(Value::Object(p.clone())).unwrap_or_default();
    let depth = as_int(&p.depth)?;
    if depth <= 0.0 {
        return None;
    }
    let age = as_int(&p.oldest_age_s)
        .map(|a| format!(", oldest {}s old", int_text(a)))
        .unwrap_or_default();
    let excerpt = match p.oldest_excerpt {
        Value::Null | Value::Bool(false) => String::new(),
        Value::String(s) => s,
        other => other.to_string(),
    };
    let mut skipped: Vec<(String, f64)> = match &p.skipped {
        Value::Object(map) => map
            .iter()
            .filter_map(|(k, v)| as_int(v).map(|n| (k.clone(), n)))
            .collect(),
        _ => Vec::new(),
    };
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    let total: f64 = skipped.iter().map(|(_, n)| n).sum();
    let mut text = format!(
        "## Operator capture: {} undispositioned operator turn(s){age}\n",
        int_text(depth)
    );
    if total != 0.0 {
        let parts: Vec<String> = skipped
            .iter()
            .map(|(k, n)| format!("{k}={}", int_text(*n)))
            .collect();
        text.push_str(&format!(
            "\nSkipped at capture: {} machine turn(s) ({})\n",
            int_text(total),
            parts.join(", ")
        ));
    }
    text.push_str("\nDisposition each before the tick ends: record with `fno backlog idea --audience internal --why ... --done-means ... --source-kind operator_request`, `fno backlog capture add`, or `fno inbox decide \"<subject>\" \"<decision>\" --audience internal --why \"<why>\"` (the law door `fno inbox decisions set` records only when the ruling retires a live law), then `fno inbox operator ack <turn-id> --outcome law:<id>|capture:<fu-id>|node:<id>|nothing`.");
    if !excerpt.is_empty() {
        text.push_str(&format!("\n\nOldest: {excerpt}"));
    }
    Some(text)
}

/// jq `type == "number" and . == floor`.
fn as_int(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite() && f.floor() == *f)
}

fn int_text(n: f64) -> String {
    format!("{n:.0}")
}

/// hook-budget.sh's `hook_run_optional`: the command under the load-aware
/// budget. A fired bound reads as silence; any other status passes through
/// with the child's stdout.
fn run_optional(argv: &[String]) -> (String, i32) {
    let budget = super::budget_secs();
    let Some(prog) = argv.first().filter(|_| budget > 0) else {
        return (String::new(), 0);
    };
    let mut cmd = Command::new(prog);
    cmd.args(&argv[1..]);
    match super::bounded(cmd, None, budget) {
        None => (String::new(), 127),
        Some(out) => match out.status.code() {
            // Killed at the bound: silence.
            None => (String::new(), 0),
            Some(code) => (
                String::from_utf8_lossy(&out.stdout)
                    .trim_end_matches('\n')
                    .to_string(),
                code,
            ),
        },
    }
}

/// hook-budget.sh's cache home: env, then platform, no config read.
fn cache_dir() -> PathBuf {
    if let Some(dir) = env_nonempty("FNO_HOOK_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if let Some(dir) = env_nonempty("FNO_CACHE_HOME") {
        let dir = match dir.strip_prefix('~') {
            Some(rest) => format!("{home}{rest}"),
            None => dir,
        };
        return PathBuf::from(format!("{dir}/hook-budget"));
    }
    if cfg!(target_os = "macos") {
        return PathBuf::from(format!("{home}/Library/Caches/fno/hook-budget"));
    }
    let xdg = env_nonempty("XDG_CACHE_HOME").unwrap_or_else(|| format!("{home}/.cache"));
    PathBuf::from(format!("{xdg}/fno/hook-budget"))
}

/// hook-budget.sh's `hook_cache_serve`: a copy young enough, built from
/// the same fingerprint, is served; past two thirds of its life it arms a
/// DETACHED refresher (`refresh` is its argv) for the next boundary. A miss
/// runs `live`, stores a non-empty answer, and a live read that skipped or
/// expired serves the stale copy rather than nothing. The status still
/// propagates. An empty fingerprint means age alone decides.
fn cache_serve(
    key: &str,
    max_age: u64,
    fp: &str,
    live: &dyn Fn() -> (String, i32),
    refresh: &[String],
) -> (String, i32) {
    if key.is_empty()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return (String::new(), 2);
    }
    let dir = cache_dir();
    let file = dir.join(key);
    let now = unix_secs(SystemTime::now());
    let mtime = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .map(unix_secs)
        .unwrap_or(0);
    let age = now - mtime;
    let raw = std::fs::read_to_string(&file).unwrap_or_default();
    let raw = raw.trim_end_matches('\n');
    let (stored_fp, payload) = raw.split_once('\n').unwrap_or(("", ""));
    if !payload.is_empty() && age >= 0 && age < max_age as i64 && (fp.is_empty() || fp == stored_fp)
    {
        if age * 3 >= max_age as i64 * 2 {
            arm_refresh(&file, fp, refresh);
        }
        return (payload.to_string(), 0);
    }
    let (out, rc) = live();
    if !out.is_empty() {
        let _ = std::fs::create_dir_all(&dir);
        prune_cache(&dir);
        store(&file, fp, &out);
        return (out, rc);
    }
    (payload.to_string(), rc)
}

/// Retention: once the dir passes 500 entries, keep the newest 250.
fn prune_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            let m = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            (m, e.path())
        })
        .collect();
    if files.len() <= 500 {
        return;
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(250) {
        let _ = std::fs::remove_file(path);
    }
}

/// `<fp>\n<out>` through a temp file and a rename, so two racing writers
/// cannot tear a read.
fn store(file: &Path, fp: &str, out: &str) {
    let tmp = PathBuf::from(format!("{}.tmp.{}", file.display(), std::process::id()));
    if std::fs::write(&tmp, format!("{fp}\n{out}")).is_ok() && std::fs::rename(&tmp, file).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn arm_refresh(file: &Path, fp: &str, refresh: &[String]) {
    let Some(bin) = super::fno_agent_bin().filter(|_| !refresh.is_empty()) else {
        return;
    };
    let mut cmd = Command::new(bin);
    cmd.args([
        "hook",
        "front",
        "Stop",
        "--leg",
        "context-nudge",
        CACHE_REFRESH,
    ])
    .arg(file)
    .arg(fp)
    .args(refresh);
    super::detach(cmd);
}

/// The detached refresher: re-run the read under the budget and store it
/// under the fingerprint captured at arm time, so a copy never outlives the
/// input state it was built from.
fn cache_refresh(args: &[String]) -> i32 {
    let (Some(file), Some(fp), Some(prog)) = (args.first(), args.get(1), args.get(2)) else {
        return 0;
    };
    let budget = super::budget_secs();
    if budget == 0 {
        return 0;
    }
    let mut cmd = Command::new(prog);
    cmd.args(&args[3..]);
    let fresh = stdout_ok(super::bounded(cmd, None, budget));
    if !fresh.is_empty() {
        store(Path::new(file), fp, &fresh);
    }
    0
}

/// The probe fingerprint: `cksum < transcript` with every non-digit mapped
/// to '-' (`930766865-9`), empty when unreadable.
fn cksum_fingerprint(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    match posix_cksum(file) {
        Some((crc, len)) => format!("{crc}-{len}"),
        None => String::new(),
    }
}

/// POSIX `cksum`: CRC-32 over polynomial 0x04C11DB7, MSB first, with the
/// length appended least-significant byte first, then complemented.
fn posix_cksum(mut input: impl Read) -> Option<(u32, u64)> {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = (i as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        *slot = c;
    }
    let step = |crc: u32, byte: u8| (crc << 8) ^ table[(((crc >> 24) as u8) ^ byte) as usize];
    let mut crc = 0u32;
    let mut len = 0u64;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        len += n as u64;
        for &b in &buf[..n] {
            crc = step(crc, b);
        }
    }
    let mut rest = len;
    while rest > 0 {
        crc = step(crc, (rest & 0xff) as u8);
        rest >>= 8;
    }
    Some((!crc, len))
}

fn git_out(repo_root: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root).args(args);
    stdout_any(super::bounded(cmd, None, CHILD_SECS))
}

/// A child's stdout whatever its exit, as `$(cmd || true)` read it.
fn stdout_any(out: Option<Output>) -> String {
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .to_string()
    })
    .unwrap_or_default()
}

fn stdout_ok(out: Option<Output>) -> String {
    stdout_any(out.filter(|o| o.status.success()))
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

fn touch(path: &Path) {
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path);
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn utc_stamp(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cksum_matches_the_posix_tool() {
        assert_eq!(posix_cksum(&b"123456789"[..]), Some((930_766_865, 9)));
        assert_eq!(posix_cksum(&b""[..]), Some((4_294_967_295, 0)));
    }

    fn row(v: Value) -> AgentRow {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn orphan_buckets_partition_by_served_liveness_and_link() {
        let rows = vec![
            row(
                json!({"name": "lead", "harness_session_id": "me", "role_level": 1, "role_scope": "x-epic"}),
            ),
            row(json!({"name": "a", "spawned_by_session": "me", "liveness": "alive"})),
            row(json!({"name": "d", "spawned_by_session": "me", "liveness": "dead"})),
            row(json!({"name": "u", "spawned_by_session": "me", "liveness": "unmeasured"})),
            row(json!({"name": "n", "spawned_by_session": "me"})),
            row(json!({"name": "loose", "liveness": "alive"})),
            row(json!({"name": "loose-q"})),
            row(json!({"name": "op", "origin": "operator", "liveness": "alive"})),
            row(
                json!({"name": "owned", "liveness": "alive", "spawn_provenance": {"kind": "team"}}),
            ),
            row(
                json!({"name": "peer", "role_level": 1, "role_scope": "other", "liveness": "alive"}),
            ),
        ];
        let b = Buckets::of(&rows, "me");
        assert_eq!(b.linked.names(), "a");
        assert_eq!(b.unknown.names(), "u, n");
        assert_eq!(b.unlinked.names(), "loose");
        assert_eq!(b.unlinked_unknown.names(), "loose-q");
        assert!(level_present(&my_row(&rows, "me").unwrap().role_level));
        assert_eq!(rollup(&rows, "x-epic"), " 1 peer lead(s) also in flight.");
        assert_eq!(
            rollup(&rows, "other/x"),
            " 2 peer lead(s) also in flight. A lead above holds your scope."
        );
        let reason = orphan_reason("x-epic", &b);
        assert!(reason
            .contains("Linked count: 1. 2 spawned worker row(s) have unresolved liveness (u, n)"));
        assert!(reason.contains("'fno agent spawn -k \"x-epic\" \"<seed prompt>\"'"));
    }

    #[test]
    fn operator_capture_reads_the_depth_object_past_a_preamble() {
        let payload = "note {not json} then {\"depth\": 2, \"oldest_age_s\": 40, \"oldest_excerpt\": \"hi\", \"skipped\": {\"b\": 1, \"a\": 2, \"c\": \"x\"}}";
        let text = operator_capture_text(payload).unwrap();
        assert!(text.starts_with("## Operator capture: 2 undispositioned operator turn(s), oldest 40s old\n\nSkipped at capture: 3 machine turn(s) (a=2, b=1)\n\nDisposition each"));
        assert!(text.ends_with("\n\nOldest: hi"));
        assert_eq!(operator_capture_text("{\"depth\": 0}"), None);
        assert_eq!(operator_capture_text("no braces"), None);
        assert!(operator_capture_text("{\"queue\": 1}")
            .unwrap()
            .contains("no depth key"));
    }
}
