//! PostToolUse legs: the session-state report, the interactive spend cap and
//! model-drift check, the claim heartbeat, and the friction offer.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) fn legs() -> Vec<super::Leg> {
    vec![
        super::Leg::new("session-state", 5, session_state),
        super::Leg::new(
            "spend-drift-monitor",
            super::CLAUDE_DEFAULT_SECS,
            spend_drift,
        ),
        super::Leg::new(
            "claim-heartbeat",
            super::CLAUDE_DEFAULT_SECS,
            claim_heartbeat,
        ),
        super::Leg::new(
            "friction-offer-inject",
            3,
            super::user_prompt::friction_offer,
        ),
    ]
}

fn session_state(_args: &[String]) -> i32 {
    if super::mux_profile() {
        super::session_state_leg("PostToolUse")
    } else {
        0
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    (now_ms() / 1000) as i64
}

/// A file's mtime in epoch seconds; 0 when it cannot be read.
fn mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `touch`: create the file when absent and set its mtime to now.
fn touch(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// `${NAME:-default}`.
fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_secs(name: &str, default: u64) -> u64 {
    env_or(name, "").parse().unwrap_or(default)
}

/// JavaScript truthiness for a JSON value.
fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `parseFloat`: the longest leading prefix that reads as a number.
fn js_parse_float(text: &str) -> Option<f64> {
    let t = text.trim_start();
    t.char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .rev()
        .find_map(|end| t[..end].parse::<f64>().ok())
}

// ---- spend-drift-monitor ----

/// At most one cost and drift check per minute.
const SPEND_THROTTLE_MS: f64 = 60_000.0;

/// The one-shot latches the spend sidecar keeps.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Latch {
    budget_warned: bool,
    budget_escalated: bool,
    drift_warned: bool,
}

impl Latch {
    fn read(state: &Map<String, Value>) -> Self {
        Latch {
            budget_warned: js_truthy(state.get("budgetWarned")),
            budget_escalated: js_truthy(state.get("budgetEscalated")),
            drift_warned: js_truthy(state.get("driftWarned")),
        }
    }

    fn write(self, state: &mut Map<String, Value>) {
        state.insert("budgetWarned".into(), json!(self.budget_warned));
        state.insert("budgetEscalated".into(), json!(self.budget_escalated));
        state.insert("driftWarned".into(), json!(self.drift_warned));
    }
}

/// Two model ids match when one is a prefix of the other once any
/// `[window]` suffix is gone. Missing data matches: never warn on absent
/// input. `<synthetic>` is the id Claude Code stamps on the error turns it
/// makes itself, not a routing answer, so it reads as absent too.
fn models_match(a: &str, b: &str) -> bool {
    static WINDOW: OnceLock<Regex> = OnceLock::new();
    let window = WINDOW.get_or_init(|| Regex::new(r"\[.*?\]").expect("window pattern"));
    let norm = |m: &str| {
        let lower = m.to_lowercase().replace("<synthetic>", "");
        window.replace_all(&lower, "").trim().to_string()
    };
    let (na, nb) = (norm(a), norm(b));
    na.is_empty() || nb.is_empty() || na.starts_with(&nb) || nb.starts_with(&na)
}

/// The one warning to give this tick, if any, and the new latch. Drift wins
/// over the budget when both would fire: a wrong model outranks the cost.
fn decide_spend_drift(
    cap_usd: Option<f64>,
    cost: Option<f64>,
    intended: Option<&str>,
    actual: Option<&str>,
    latch: Latch,
) -> (Option<String>, Latch) {
    let mut s = latch;
    if let (Some(intended), Some(actual)) = (intended, actual) {
        if !s.drift_warned && !models_match(intended, actual) {
            s.drift_warned = true;
            return (
                Some(format!(
                    "MODEL DRIFT: attested '{intended}' but this session is running \
                     '{actual}'. A routing failure slipped past the SessionStart env \
                     check (class). Verify your provider routing before trusting this run."
                )),
                s,
            );
        }
    }
    if let (Some(cap), Some(cost)) = (cap_usd, cost) {
        if cost >= 2.0 * cap && !s.budget_escalated {
            s.budget_escalated = true;
            s.budget_warned = true;
            return (
                Some(format!(
                    "BUDGET CAP EXCEEDED (2x): this interactive session has spent \
                     ${cost:.2}, over 2x the ${cap:.2} cap \
                     (config.budget.interactive.cap_usd). Consider wrapping up or starting fresh."
                )),
                s,
            );
        }
        if cost >= cap && !s.budget_warned {
            s.budget_warned = true;
            return (
                Some(format!(
                    "BUDGET CAP: this interactive session has spent ${cost:.2} of the \
                     ${cap:.2} cap (config.budget.interactive.cap_usd). Advisory only; \
                     the session is not blocked."
                )),
                s,
            );
        }
    }
    (None, s)
}

/// `config.budget.interactive.cap_usd`, read from the raw TOML (local, then
/// global): the key is unmodeled, so `fno config get` refuses it. The
/// line-anchored match keeps `cost_cap_usd =` from reading as the cap.
fn read_interactive_cap() -> Option<f64> {
    static CAP: OnceLock<Regex> = OnceLock::new();
    let cap = CAP.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*cap_usd[ \t]*=[ \t]*([0-9.]+)").expect("cap pattern")
    });
    let mut files = vec![PathBuf::from(".fno/config.toml")];
    if let Some(home) = std::env::var_os("HOME") {
        files.push(PathBuf::from(home).join(".fno").join("config.toml"));
    }
    files.iter().find_map(|p| {
        let content = std::fs::read_to_string(p).ok()?;
        let n = js_parse_float(cap.captures(&content)?.get(1)?.as_str())?;
        (n.is_finite() && n > 0.0).then_some(n)
    })
}

fn fno_json(args: &[&str], secs: u64) -> Option<Value> {
    let mut cmd = Command::new(super::fno_bin()?);
    cmd.args(args);
    serde_json::from_str(&super::bounded_stdout(cmd, None, secs)?).ok()
}

/// Spend so far from the transcript; None on any failure.
fn probe_cost(session: &str) -> Option<f64> {
    let j = fno_json(&["cost", session, "--json"], 10)?;
    let c = match j.get("cost_usd")? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => js_parse_float(s),
        _ => None,
    }?;
    c.is_finite().then_some(c)
}

/// The running model from the transcript; None on any failure.
fn probe_model(transcript: Option<&str>) -> Option<String> {
    let transcript = transcript.filter(|t| !t.is_empty())?;
    let j = fno_json(&["context", "--transcript", transcript, "--json"], 8)?;
    j.get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

fn attested_model(session: &str) -> Option<String> {
    let home = std::env::var_os("FNO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))?;
    let raw = std::fs::read_to_string(home.join("attest").join(format!("{session}.json"))).ok()?;
    serde_json::from_str::<Value>(&raw)
        .ok()?
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

fn persist(path: &Path, state: &Map<String, Value>) {
    let _ = std::fs::write(path, Value::Object(state.clone()).to_string());
}

fn spend_drift(_args: &[String]) -> i32 {
    let (_, payload) = super::payload();
    let session = payload.session();
    if session.is_empty() {
        return 0;
    }
    let spend_path = std::env::temp_dir().join(format!("claude-ctx-{session}-spend.json"));
    let mut state = Map::new();
    state.insert("lastTs".into(), json!(0));
    Latch::default().write(&mut state);
    // A corrupt sidecar re-checks rather than going silent.
    if let Some(Value::Object(saved)) = std::fs::read_to_string(&spend_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
    {
        state.extend(saved);
    }

    let now = now_ms();
    let last = state.get("lastTs").and_then(Value::as_f64).unwrap_or(0.0);
    if (now as f64) - last < SPEND_THROTTLE_MS {
        return 0;
    }
    let cap = read_interactive_cap();
    let prior = Latch::read(&state);
    let intended = if prior.drift_warned {
        None
    } else {
        attested_model(session)
    };
    state.insert("lastTs".into(), json!(now));
    // With no cap and no drift target there is nothing to check, and no
    // cost call is made.
    if cap.is_none() && intended.is_none() {
        persist(&spend_path, &state);
        return 0;
    }
    let actual = intended
        .as_ref()
        .and_then(|_| probe_model(payload.transcript_path.as_deref()));
    let cost = cap.and_then(|_| probe_cost(session));
    let (message, latch) =
        decide_spend_drift(cap, cost, intended.as_deref(), actual.as_deref(), prior);
    match message {
        None => {
            latch.write(&mut state);
            persist(&spend_path, &state);
        }
        Some(message) => {
            // The advanced throttle is saved first with the prior latch, so
            // a warning that never reached the reader warns again.
            persist(&spend_path, &state);
            super::emit_context("PostToolUse", &message);
            latch.write(&mut state);
            persist(&spend_path, &state);
        }
    }
    0
}

// ---- claim-heartbeat ----

/// The activity stamp refreshes at most this often. It must stay inside the
/// peer reader's 120 s window (hooks/helpers/worktree-live-peers.sh).
const LIVE_THROTTLE_SECS: i64 = 30;

/// The claim states `claim status` answers.
const CLAIM_STATES: [&str; 6] = ["free", "live", "suspect", "stale", "corrupted", "unknown"];

/// jq `-r '.field // empty'`: absent, null and false read as empty.
fn jq_text(v: Option<&Value>, field: &str) -> String {
    match v.and_then(|v| v.get(field)) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// An integral, non-negative `expires_at`, else nothing.
fn integral_expires(v: Option<&Value>) -> Option<i64> {
    let n = v?.get("expires_at")?;
    if let Some(i) = n.as_i64() {
        return (i >= 0).then_some(i);
    }
    let f = n.as_f64()?;
    (f.fract() == 0.0 && f >= 0.0 && f < i64::MAX as f64).then_some(f as i64)
}

fn state_in(v: Option<&Value>, states: &[&str]) -> bool {
    v.and_then(|v| v.get("state"))
        .and_then(Value::as_str)
        .is_some_and(|s| states.contains(&s))
}

/// Run `work` on its own thread and wait at most `secs` for it. A call past
/// its bound answers None and is left to finish on its own.
fn within<T: Send + 'static>(secs: u64, work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("claim-heartbeat-call".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .ok()?;
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}

/// The record-only status of `key` (`claim status --json --no-roster`),
/// bounded. None is an unreadable instrument.
fn claim_status(key: &str) -> Option<Value> {
    let secs = env_secs("FNO_CLAIM_HEARTBEAT_STATUS_TIMEOUT", 5);
    let key = key.to_string();
    within(secs, move || {
        let root = crate::claim_cli::node_aware_root(&key);
        crate::claim_cli::status::record_payload(&key, root.as_deref())
    })
}

/// What one `claim refresh` answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RefreshAnswer {
    /// The leaf's exit code; 124 is the bound firing.
    Failed(i32),
    /// The PID-liveness no-op (`refreshed: false`).
    NoOp,
    /// Renewed, with the deadline the receipt echoed.
    Renewed(Option<i64>),
}

fn claim_refresh(key: &str, holder: &str, ttl: &str) -> RefreshAnswer {
    use crate::claim_cli::refresh::{refresh, Refreshed};
    let secs = env_secs("FNO_CLAIM_HEARTBEAT_REFRESH_TIMEOUT", 5);
    let (key, holder, ttl) = (key.to_string(), holder.to_string(), ttl.to_string());
    match within(secs, move || refresh(&key, &holder, &ttl)) {
        None => RefreshAnswer::Failed(124),
        Some(Err(code)) => RefreshAnswer::Failed(code),
        Some(Ok(Refreshed::NoOp)) => RefreshAnswer::NoOp,
        Some(Ok(Refreshed::Renewed(r))) => RefreshAnswer::Renewed(r.expires_at.filter(|e| *e >= 0)),
    }
}

/// The manifest fields the heartbeat reads.
#[derive(Debug, Default, PartialEq, Eq)]
struct Manifest {
    node: String,
    session: String,
    holder: String,
    claude_sid: String,
    codex_thread: String,
}

/// `sed -n 's/^[[:space:]]*<name>:[[:space:]]*//p' | head -1 | tr -d "\"'"`.
fn manifest_field(text: &str, name: &str) -> String {
    text.lines()
        .find_map(|line| {
            line.trim_start()
                .strip_prefix(name)?
                .strip_prefix(':')
                .map(|rest| rest.trim_start().replace(['"', '\''], ""))
        })
        .unwrap_or_default()
}

/// graph_node_id lives in the manifest body, session_id in the frontmatter.
/// None when either is missing: no target session to renew.
fn parse_manifest(text: &str) -> Option<Manifest> {
    let node = manifest_field(text, "graph_node_id");
    let session = manifest_field(text, "session_id");
    if node.is_empty() || node == "null" || session.is_empty() {
        return None;
    }
    let mut holder = manifest_field(text, "target_claim_holder");
    if holder.is_empty() || holder == "null" {
        holder = format!("target-session:{session}");
    }
    let mut codex_thread = manifest_field(text, "codex_thread_id");
    if codex_thread == "null" {
        codex_thread.clear();
    }
    Some(Manifest {
        node,
        session,
        holder,
        claude_sid: manifest_field(text, "claude_session_id"),
        codex_thread,
    })
}

/// Who is running this hook.
struct Identity<'a> {
    codex: bool,
    claude_sid: &'a str,
    codex_thread: &'a str,
}

/// Prove the running session owns the manifest, not a stale one a dead
/// session left in this worktree: a positive mismatch would revive the dead
/// session's claim. Claude fails open when its identity is unknown. Codex
/// fails closed when either identity is missing: a generic holder proves no
/// ownership.
fn owns(m: &Manifest, id: &Identity) -> bool {
    if !id.codex {
        return id.claude_sid.is_empty()
            || m.claude_sid.is_empty()
            || m.claude_sid == "null"
            || id.claude_sid == m.claude_sid;
    }
    let compact: String = id.codex_thread.split_whitespace().collect();
    !compact.is_empty()
        && !m.codex_thread.is_empty()
        && id.codex_thread == m.codex_thread
        && (m.holder == format!("target-session:{}", m.codex_thread)
            || m.holder == format!("target-session:{}", m.session))
}

/// The session the ledger names: the holder's own credential when it is a
/// `target-session:<id>`, else the harness id. Never the pid: a harness
/// restart changes it while the session id holds.
fn ledger_sid(m: &Manifest) -> String {
    let mut sid = match m.holder.strip_prefix("target-session:") {
        Some(rest) if !rest.is_empty() && rest != "null" => rest.to_string(),
        _ => m.claude_sid.clone(),
    };
    if sid.is_empty() || sid == "null" {
        sid = m.session.clone();
    }
    sid
}

/// The holder gate over one status read: the pre-read deadline when this
/// session holds the claim, else the ledger outcome and its stderr line.
/// Garbage from the instrument is unreadable, never "no claim".
fn holder_gate(
    status: Option<&Value>,
    m: &Manifest,
    sid: &str,
) -> Result<Option<i64>, (&'static str, String)> {
    let node = &m.node;
    let shape_ok = status.is_some_and(Value::is_object) && state_in(status, &CLAIM_STATES);
    if !shape_ok {
        return Err((
            "status_unreadable",
            format!("claim-heartbeat: claim status unreadable for node:{node}; renewal UNVERIFIED this window"),
        ));
    }
    let state = jq_text(status, "state");
    if state == "corrupted" {
        return Err((
            "corrupted",
            format!("claim-heartbeat: node:{node} claim file corrupted; ownership unverifiable, renewal impossible this window"),
        ));
    }
    let holder = jq_text(status, "holder");
    if holder != m.holder {
        if holder.is_empty() || state == "free" || state == "stale" {
            return Err((
                "no_claim",
                format!("claim-heartbeat: node:{node} NOT held by session {sid} (state={state}) - work is UNPROTECTED; take a claim or leave this node"),
            ));
        }
        return Err((
            "not_holder",
            format!("claim-heartbeat: node:{node} held by {holder} (state={state}), not by this session - split-brain or handover in progress"),
        ));
    }
    Ok(integral_expires(status))
}

/// One renewal's ledger outcome, the deadline it records, and its stderr
/// line. A renewal counts only when a re-read of the store shows the
/// deadline past both the receipt and the pre-read: an exit 0 proves
/// nothing.
fn renewal_outcome(
    answer: RefreshAnswer,
    node: &str,
    pre: Option<i64>,
    post: Option<i64>,
) -> (&'static str, Option<i64>, Option<String>) {
    match answer {
        RefreshAnswer::Failed(rc) => (
            "refresh_failed",
            None,
            Some(format!("claim-heartbeat: refresh failed for node:{node} rc={rc} (1 write failure, 2 expired/refused, 3 gone/corrupted, 4 holder mismatch, 124 timeout; non-fatal)")),
        ),
        RefreshAnswer::NoOp => (
            "refresh_unverified",
            None,
            Some(format!("claim-heartbeat: refresh of node:{node} was a no-op (PID-liveness claim); TTL not extended")),
        ),
        RefreshAnswer::Renewed(echoed) => match (echoed, post) {
            (Some(new), Some(post)) if post >= new && pre.is_none_or(|pre| post > pre) => {
                ("renewed", Some(post), None)
            }
            _ => {
                let show = |v: Option<i64>| v.map_or("unset".to_string(), |v| v.to_string());
                (
                    "refresh_unverified",
                    None,
                    Some(format!(
                        "claim-heartbeat: refresh of node:{node} exited 0 but the store disagrees (pre={} echoed={} re-read={}); renewal UNVERIFIED",
                        show(pre),
                        show(echoed),
                        show(post)
                    )),
                )
            }
        },
    }
}

/// Whether a handover refresh landed: the re-read names the same holder,
/// live or suspect, with a deadline past the recorded one.
fn handover_renewed(after: Option<&Value>, holder: &str, recorded: i64) -> bool {
    after.is_some_and(Value::is_object)
        && state_in(after, &["live", "suspect"])
        && after.and_then(|a| a.get("holder")).and_then(Value::as_str) == Some(holder)
        && integral_expires(after).is_some_and(|e| e > recorded)
}

/// One-snapshot outcome ledger, written atomically beside the manifest:
/// key=value lines for a human or grep mid-incident.
fn write_stamp(
    stamp: &Path,
    outcome: &str,
    m: &Manifest,
    sid: &str,
    holder_state: &str,
    expires: Option<i64>,
) {
    let holder_state = if holder_state.is_empty() {
        "unknown"
    } else {
        holder_state
    };
    let mut body = format!(
        "ts={}\noutcome={outcome}\nnode={}\nsession_id={sid}\nholder_state={holder_state}\n",
        now_secs(),
        m.node
    );
    if let Some(e) = expires {
        body.push_str(&format!("expires_at={e}\n"));
    }
    let tmp = PathBuf::from(format!("{}.tmp.{}", stamp.display(), std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, stamp);
    }
    let _ = std::fs::remove_file(&tmp);
}

/// `<git dir>/fno/live`: the per-physical-worktree activity folder, even
/// when worktrees share one `.fno`.
fn live_dir(cwd: &str) -> Option<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").args([
        "-C",
        cwd,
        "rev-parse",
        "--absolute-git-dir",
    ]);
    let git_dir = super::bounded_stdout(cmd, None, 3)?;
    let git_dir = PathBuf::from(git_dir.trim());
    (!git_dir.as_os_str().is_empty() && git_dir.is_dir()).then(|| git_dir.join("fno").join("live"))
}

/// Whether a stamp younger than `throttle` seconds stands.
fn fresh(stamp: &Path, throttle: i64) -> bool {
    let (now, mtime) = (now_secs(), mtime_secs(stamp));
    stamp.is_file() && now > 0 && mtime > 0 && now - mtime < throttle
}

fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A failed or errored tool result: `is_error`/`isError` true, or a nonzero
/// `exit_code`/`exitCode`.
fn tool_failed(response: Option<&Value>) -> bool {
    let Some(Value::Object(r)) = response else {
        return false;
    };
    let first = |a: &str, b: &str| {
        [a, b].iter().find_map(|k| {
            r.get(*k)
                .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
        })
    };
    let errored = first("is_error", "isError") == Some(&Value::Bool(true));
    let code = match first("exit_code", "exitCode") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    };
    errored || code != 0.0
}

/// Every distinct GitHub PR URL anywhere in the tool result's strings.
fn pr_urls(response: Option<&Value>) -> BTreeSet<String> {
    static URL: OnceLock<Regex> = OnceLock::new();
    let url = URL.get_or_init(|| {
        Regex::new(r"https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/pull/[0-9]+")
            .expect("pr url pattern")
    });
    fn walk(v: &Value, url: &Regex, out: &mut BTreeSet<String>) {
        match v {
            Value::String(s) => out.extend(url.find_iter(s).map(|m| m.as_str().to_string())),
            Value::Array(a) => a.iter().for_each(|v| walk(v, url, out)),
            Value::Object(o) => o.values().for_each(|v| walk(v, url, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    if let Some(r) = response {
        walk(r, url, &mut out);
    }
    out
}

/// The binder's refusal text: `.refusal // .error` of its JSON, else its
/// first line.
fn bind_reason(output: &str, rc: i32) -> String {
    if rc == 124 {
        return "timed out".into();
    }
    let parsed = std::iter::once(output.trim())
        .chain(output.lines())
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .map(|v| {
            let refusal = jq_text(Some(&v), "refusal");
            if refusal.is_empty() {
                jq_text(Some(&v), "error")
            } else {
                refusal
            }
        })
        .find(|r| !r.is_empty());
    parsed
        .or_else(|| {
            output
                .lines()
                .next()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
        })
        .unwrap_or_else(|| format!("binder exited {rc}"))
}

/// A raw `gh pr create` bypasses the fno PR path, but its successful URL
/// passes here. Bind only one unambiguous PR URL; the binder then checks the
/// branch names exactly one real node. Every refusal is non-fatal.
fn bind_created_pr(command: &str, response: Option<&Value>, cwd: &str) {
    match crate::hook::pr_attribution::attribution(command) {
        "bare" => {}
        word @ ("compound" | "invalid") => {
            eprintln!(
                "claim-heartbeat: gh pr create not attributable ({word} command); binding skipped"
            );
            return;
        }
        _ => return,
    }
    let urls = pr_urls(response);
    if tool_failed(response) {
        eprintln!("claim-heartbeat: gh pr create failed; binding skipped");
        return;
    }
    let url = match urls.len() {
        0 => {
            eprintln!("claim-heartbeat: gh pr create returned no PR URL; binding skipped");
            return;
        }
        1 => urls.into_iter().next().unwrap_or_default(),
        _ => {
            eprintln!("claim-heartbeat: gh pr create returned ambiguous PR URLs; binding skipped");
            return;
        }
    };
    let secs = env_secs("FNO_PR_BIND_CREATED_TIMEOUT", 2);
    let payload = json!({"url": url, "repo": cwd, "node": Value::Null});
    let (mut rc, mut output) = (1, String::new());
    for _ in 0..2 {
        let p = payload.clone();
        (rc, output) = match within(secs, move || {
            crate::org_board::pr_closure_produce::leg_bind(&p)
        }) {
            Some((code, out, err)) => (code, out + &err),
            None => (124, String::new()),
        };
        if rc == 0 {
            return;
        }
    }
    eprintln!(
        "claim-heartbeat: PR binding failed after 2 attempts: {}; run: fno do pr bind-created --url {url} --repo {cwd}",
        bind_reason(&output, rc)
    );
}

/// Before target init there is no manifest, but an explicit spawn already
/// owns a 15-minute handover lease. Keep only that exact holder alive while
/// its worker works. `refresh` can extend; it cannot acquire or steal.
fn handover_heartbeat(cwd: &str, live: Option<&Path>) {
    static NODE: OnceLock<Regex> = OnceLock::new();
    let node_shape =
        NODE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9]{0,7}-[0-9a-f]{4,8}$").expect("node"));
    let node = std::env::var("FNO_NODE").unwrap_or_default();
    let holder = std::env::var("FNO_NODE_CLAIM_HOLDER").unwrap_or_default();
    if node.is_empty() || holder.is_empty() {
        return;
    }
    if !node_shape.is_match(&node)
        || !holder.starts_with("spawn-handover:")
        || holder == "spawn-handover:"
    {
        return;
    }
    // A HOME cwd outside git makes $CWD/.fno the state root itself; such a
    // session holds no repo binding, so the leg skips.
    let repo_fno = std::fs::canonicalize(Path::new(cwd).join(".fno")).ok();
    let state = std::fs::canonicalize(PathBuf::from(env_or(
        "FNO_HOME",
        &format!("{}/.fno", env_or("HOME", "")),
    )))
    .ok();
    if repo_fno.is_some_and(|r| r.is_dir() && Some(&r) == state.as_ref()) {
        return;
    }
    // A linked worktree has no $CWD/.fno; the git dir's fno/ folder is per
    // physical worktree, so the throttle stamp can always be written there.
    let stamp = match live.and_then(Path::parent) {
        Some(fno) => {
            let _ = std::fs::create_dir_all(fno);
            fno.join("claim-handover-heartbeat.stamp")
        }
        None => Path::new(cwd).join(".fno/.claim-handover-heartbeat.stamp"),
    };
    let throttle = env_secs("FNO_CLAIM_HANDOVER_HEARTBEAT_THROTTLE", 300) as i64;
    if fresh(&stamp, throttle) {
        return;
    }
    let key = format!("node:{node}");
    let status = claim_status(&key);
    let valid = status.as_ref().is_some_and(|s| {
        s.is_object()
            && state_in(Some(s), &CLAIM_STATES)
            && matches!(
                s.get("holder"),
                None | Some(Value::Null) | Some(Value::String(_))
            )
    });
    if !valid {
        eprintln!(
            "claim-heartbeat: handover claim status unreadable for {key}; refresh remains due"
        );
        return;
    }
    // The spawner exits before init, so its unexpired handover reads
    // suspect; the TTL still protects it and permits renewal.
    if !(state_in(status.as_ref(), &["live", "suspect"])
        && jq_text(status.as_ref(), "holder") == holder)
    {
        // A complete answer proves there is nothing this holder may
        // refresh: throttle the safe no-op.
        touch(&stamp);
        return;
    }
    let Some(recorded) = integral_expires(status.as_ref()) else {
        eprintln!("claim-heartbeat: handover ownership-lost/refresh-not-confirmed for {key} (missing deadline); refresh remains due");
        return;
    };
    let ttl = env_or("FNO_CLAIM_HANDOVER_TTL", "15m");
    match claim_refresh(&key, &holder, &ttl) {
        RefreshAnswer::Failed(_) => {
            eprintln!("claim-heartbeat: handover refresh failed for {key}; refresh remains due")
        }
        _ if handover_renewed(claim_status(&key).as_ref(), &holder, recorded) => touch(&stamp),
        _ => eprintln!("claim-heartbeat: handover ownership-lost/refresh-not-confirmed for {key}; refresh remains due"),
    }
}

/// Renew this session's `node:<id>` claim TTL while it works, so a live
/// session never loses its claim at the 2h mark. Holder-gated: it never
/// revives or steals another session's claim. Throttled by a stamp beside
/// the manifest, which is also the outcome ledger: every cycle names what it
/// did. Never blocks the tool call: every path answers 0 with no stdout.
fn claim_heartbeat(_args: &[String]) -> i32 {
    let raw = crate::hook::read_stdin();
    let input = serde_json::from_str::<Value>(raw.trim()).ok();
    let text = |field: &str| -> String {
        match input.as_ref().and_then(|v| v.get(field)) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }
    };
    let hook_sid = input
        .as_ref()
        .and_then(|v| v.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_name = text("tool_name");
    let shell_tool = matches!(tool_name.as_str(), "Bash" | "Shell" | "exec_command");
    let tool_command = if shell_tool {
        let tool_input = input.as_ref().and_then(|v| v.get("tool_input"));
        // jq's `.command // .cmd`: only null, false or absent fall through.
        match tool_input.and_then(|t| t.get("command")) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => jq_text(tool_input, "cmd"),
            Some(_) => jq_text(tool_input, "command"),
        }
    } else {
        String::new()
    };
    let mut cwd = text("cwd");
    if cwd.is_empty() {
        cwd = std::env::var("CLAUDE_PROJECT_DIR")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| super::cwd().display().to_string());
    }

    let env_thread = std::env::var("CODEX_THREAD_ID").unwrap_or_default();
    let codex = std::env::var("FNO_PLATFORM").as_deref() == Ok("codex")
        || std::env::var_os("CODEX_PLUGIN_ROOT").is_some_and(|v| !v.is_empty())
        || env_thread.split_whitespace().next().is_some();
    let (claude_sid, codex_thread) = if codex {
        let thread = if env_thread.split_whitespace().next().is_some() {
            env_thread
        } else {
            hook_sid.clone()
        };
        (String::new(), thread)
    } else {
        (hook_sid.clone(), env_thread)
    };

    // Stamp activity before the manifest and holder gates: a non-owner's
    // activity is still overlap.
    let live = live_dir(&cwd);
    let live_id = if codex { &codex_thread } else { &claude_sid };
    if let Some(live) = live.as_deref().filter(|_| plain_id(live_id)) {
        let stamp = live.join(live_id);
        let (now, mtime) = (now_secs(), mtime_secs(&stamp));
        if (now <= 0 || mtime <= 0 || now - mtime >= LIVE_THROTTLE_SECS)
            && std::fs::create_dir_all(live).is_ok()
        {
            touch(&stamp);
        }
    }

    if shell_tool
        && tool_command.contains("gh")
        && tool_command.contains("pr")
        && tool_command.contains("create")
    {
        let response = input.as_ref().and_then(|v| v.get("tool_response"));
        bind_created_pr(&tool_command, response, &cwd);
    }

    handover_heartbeat(&cwd, live.as_deref());

    let here = super::cwd();
    let here = std::fs::canonicalize(&here).unwrap_or(here);
    let manifest_path = crate::state_path::resolve("target-state", &here)
        .unwrap_or_else(|| Path::new(&cwd).join(".fno/target-state.md"));
    if !manifest_path.is_file() {
        return 0;
    }
    let Some(m) = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|t| parse_manifest(&t))
    else {
        return 0;
    };
    let id = Identity {
        codex,
        claude_sid: &claude_sid,
        codex_thread: &codex_thread,
    };
    if !owns(&m, &id) {
        return 0;
    }
    let sid = ledger_sid(&m);

    // The stamp sits beside the resolved manifest: a linked worktree has no
    // $CWD/.fno, and a stamp that cannot be written never throttles.
    let stamp = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".claim-heartbeat.stamp");
    let throttle = env_secs("FNO_CLAIM_HEARTBEAT_THROTTLE", 1200) as i64;
    if fresh(&stamp, throttle) {
        return 0;
    }

    let key = format!("node:{}", m.node);
    let status = claim_status(&key);
    let holder_state = jq_text(status.as_ref(), "state");
    let pre = match holder_gate(status.as_ref(), &m, &sid) {
        Ok(pre) => pre,
        Err((outcome, line)) => {
            eprintln!("{line}");
            write_stamp(&stamp, outcome, &m, &sid, &holder_state, None);
            return 0;
        }
    };
    // Re-arm to the claim's 2h window: refresh with no ttl shrinks it to
    // one minute.
    let ttl = env_or("FNO_CLAIM_HEARTBEAT_TTL", "2h");
    let answer = claim_refresh(&key, &m.holder, &ttl);
    let post = match answer {
        RefreshAnswer::Renewed(_) => integral_expires(claim_status(&key).as_ref()),
        _ => None,
    };
    let (outcome, expires, line) = renewal_outcome(answer, &m.node, pre, post);
    if let Some(line) = line {
        eprintln!("{line}");
    }
    write_stamp(&stamp, outcome, &m, &sid, &holder_state, expires);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_drift_warns_once_per_latch() {
        assert!(models_match("claude-opus-4-8", "claude-opus-4-8[1m]"));
        assert!(!models_match("glm-4.6", "claude-opus-4-8"));
        assert!(!models_match("claude-opus-4-8", "claude-sonnet-4-5"));
        assert!(models_match("", "claude-opus-4-8"));
        assert!(models_match("claude-opus-5", "<synthetic>"));

        let fresh = Latch::default();
        let decide = decide_spend_drift;
        assert_eq!(decide(None, Some(999.0), None, None, fresh).0, None);
        assert_eq!(decide(Some(5.0), Some(4.99), None, None, fresh).0, None);
        let (msg, at_cap) = decide(Some(5.0), Some(5.0), None, None, fresh);
        let msg = msg.expect("cost == cap warns");
        assert!(msg.starts_with("BUDGET CAP: ") && msg.contains("$5.00 of the $5.00 cap"));
        assert!(at_cap.budget_warned && !at_cap.budget_escalated);
        assert_eq!(decide(Some(5.0), Some(5.5), None, None, at_cap).0, None);
        let (msg, doubled) = decide(Some(5.0), Some(10.5), None, None, fresh);
        assert!(msg.expect("2x warns").contains("(2x)"));
        assert!(doubled.budget_escalated && doubled.budget_warned);
        let (msg, drift) = decide(
            Some(5.0),
            Some(99.0),
            Some("glm-4.6"),
            Some("claude-opus-4-8"),
            fresh,
        );
        assert_eq!(
            msg.as_deref(),
            Some(
                "MODEL DRIFT: attested 'glm-4.6' but this session is running 'claude-opus-4-8'. \
                 A routing failure slipped past the SessionStart env check (class). \
                 Verify your provider routing before trusting this run."
            )
        );
        assert!(drift.drift_warned && !drift.budget_warned);
        let (msg, _) = decide(
            Some(5.0),
            Some(6.0),
            Some("claude-opus-4-8"),
            Some("claude-opus-4-8[1m]"),
            fresh,
        );
        assert!(msg
            .expect("coherent model falls to budget")
            .starts_with("BUDGET CAP: "));
        assert_eq!(
            decide(None, None, Some("glm-4.6"), Some("claude-opus-4-8"), drift).0,
            None
        );
        assert_eq!(js_parse_float("5.0.1"), Some(5.0));
        assert_eq!(js_parse_float("."), None);
    }

    #[test]
    fn heartbeat_reads_manifest_identity_and_pr_evidence() {
        let text = "---\nsession_id: 20260707T203700Z-cl55246-f3fe72\n\
            claude_session_id: 182b29c8-owner-uuid\ncodex_thread_id: 019f48e4-owner-thread\n\
            ---\n# Mission\ngraph_node_id: x-aaaa\n\
            target_claim_holder: \"target-session:20260707T203700Z-cl55246-f3fe72\"\n";
        let m = parse_manifest(text).expect("manifest");
        assert_eq!(m.node, "x-aaaa");
        assert_eq!(m.holder, "target-session:20260707T203700Z-cl55246-f3fe72");
        assert_eq!(ledger_sid(&m), "20260707T203700Z-cl55246-f3fe72");
        assert_eq!(parse_manifest("session_id: s\ngraph_node_id: null\n"), None);
        let bare = parse_manifest("session_id: s1\ngraph_node_id: x-1\nclaude_session_id: c1\n")
            .expect("bare manifest");
        assert_eq!(bare.holder, "target-session:s1");

        fn claude(sid: &str) -> Identity<'_> {
            Identity {
                codex: false,
                claude_sid: sid,
                codex_thread: "",
            }
        }
        fn codex(thread: &str) -> Identity<'_> {
            Identity {
                codex: true,
                claude_sid: "",
                codex_thread: thread,
            }
        }
        // Claude: a match or an unknown identity owns; a mismatch never does.
        assert!(owns(&m, &claude("182b29c8-owner-uuid")));
        assert!(owns(&m, &claude("")));
        assert!(!owns(&m, &claude("other-uuid")));
        // Codex: the thread must match, and an absent identity fails closed.
        assert!(owns(&m, &codex("019f48e4-owner-thread")));
        let foreign = Manifest {
            holder: "target-session:someone-else".into(),
            ..parse_manifest(text).expect("manifest")
        };
        assert!(!owns(&foreign, &codex("019f48e4-owner-thread")));
        let codex_m = Manifest {
            holder: "target-session:019f48e4-owner-thread".into(),
            ..parse_manifest(text).expect("manifest")
        };
        assert!(owns(&codex_m, &codex("019f48e4-owner-thread")));
        assert!(!owns(&codex_m, &codex("other-thread")));
        assert!(!owns(&codex_m, &codex("  ")));
        let legacy = Manifest {
            codex_thread: String::new(),
            ..parse_manifest(text).expect("manifest")
        };
        assert!(!owns(&legacy, &codex("019f48e4-owner-thread")));

        let url = "https://github.com/o/r/pull/7";
        let ok = json!({"stdout": format!("{url}\n"), "exit_code": 0});
        assert_eq!(pr_urls(Some(&ok)).len(), 1);
        assert!(!tool_failed(Some(&ok)));
        assert!(tool_failed(Some(&json!({"stdout": url, "exitCode": "1"}))));
        assert!(tool_failed(Some(&json!({"isError": true}))));
        let two = json!({"stdout": format!("{url} https://github.com/o/r/pull/8 {url}")});
        assert_eq!(pr_urls(Some(&two)).len(), 2);
        assert_eq!(bind_reason("{\"refusal\":\"no node\"}\n", 1), "no node");
        assert_eq!(bind_reason("", 124), "timed out");
        assert_eq!(bind_reason("", 3), "binder exited 3");
    }

    #[test]
    fn heartbeat_names_every_cycle_outcome() {
        let m = parse_manifest("session_id: s1\ngraph_node_id: x-1\n").expect("manifest");
        let gate = |v: Value| holder_gate(Some(&v), &m, "s1").map_err(|(o, _)| o);
        assert_eq!(
            holder_gate(None, &m, "s1").map_err(|(o, _)| o),
            Err("status_unreadable")
        );
        assert_eq!(gate(json!("Traceback")), Err("status_unreadable"));
        assert_eq!(gate(json!({"state": "weird"})), Err("status_unreadable"));
        assert_eq!(gate(json!({"state": "corrupted"})), Err("corrupted"));
        assert_eq!(gate(json!({"state": "free"})), Err("no_claim"));
        let (_, line) = holder_gate(Some(&json!({"state": "free"})), &m, "s1").unwrap_err();
        assert!(line.contains("UNPROTECTED") && line.contains("(state=free)"));
        assert_eq!(
            gate(json!({"state": "live", "holder": "other"})),
            Err("not_holder")
        );
        let ours = json!({"state": "live", "holder": "target-session:s1", "expires_at": 100});
        assert_eq!(gate(ours), Ok(Some(100)));

        let outcome = |a, pre, post| {
            let (o, e, _) = renewal_outcome(a, "x-1", pre, post);
            (o, e)
        };
        use RefreshAnswer::*;
        assert_eq!(
            outcome(Renewed(Some(200)), Some(100), Some(200)),
            ("renewed", Some(200))
        );
        assert_eq!(
            outcome(Renewed(Some(200)), None, Some(200)),
            ("renewed", Some(200))
        );
        assert_eq!(
            outcome(Renewed(Some(200)), Some(100), Some(100)),
            ("refresh_unverified", None)
        );
        assert_eq!(
            outcome(Renewed(Some(100)), Some(100), Some(100)),
            ("refresh_unverified", None)
        );
        assert_eq!(
            outcome(Renewed(None), Some(100), Some(200)),
            ("refresh_unverified", None)
        );
        assert_eq!(outcome(NoOp, Some(100), None), ("refresh_unverified", None));
        let (o, _, line) = renewal_outcome(Failed(4), "x-1", None, None);
        assert_eq!(o, "refresh_failed");
        assert!(line.expect("diagnostic").contains("rc=4"));

        let after = json!({"state": "suspect", "holder": "spawn-handover:a", "expires_at": 200});
        assert!(handover_renewed(Some(&after), "spawn-handover:a", 100));
        assert!(!handover_renewed(Some(&after), "spawn-handover:b", 100));
        assert!(!handover_renewed(Some(&after), "spawn-handover:a", 200));
        assert!(!handover_renewed(
            Some(&json!({"state": "stale", "holder": "spawn-handover:a", "expires_at": 300})),
            "spawn-handover:a",
            100
        ));
    }
}
