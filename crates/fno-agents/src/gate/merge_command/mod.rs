//! Native `fno do pr merge`: the argv flow from parse through the merge lock
//! and the authorized-merge decide/effect passes.
//!
//! Staged port. The incarnation fence is a silent no-op at this stage (it
//! lands with the fence stage; absence fails open exactly like a crashed
//! check does on the Python leg). The coverage pre-read and the gh-backed
//! status publish stay Python-side until the follow-ups stage, so the covered
//! head this flow pins is the empty string end to end. The merged-branch
//! cleanup (note fetch, confirmed-merge finish, graph sync) also stays
//! Python-side; no captured golden at this stage reaches it.
//!
//! The flow is transport-only: every merge-permission decision lives in
//! `authorized_merge.rs`, reached through the same `authorized-merge` command
//! hop the terminal arm uses, so this command and the terminal arm cannot
//! disagree about who may merge.

use serde_json::{json, Map, Value};
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MERGE_LOCK_WAIT_S: u64 = 120;
const MERGE_LOCK_POLL_S: u64 = 5;
const FLAKE_PROBE_TIMEOUT: Duration = Duration::from_secs(120);
const BIN_MISSING: &str =
    "the fno-agents binary was not found; reinstall fno, run `fno doctor update --rust`, or set FNO_AGENTS_BIN";

/// The merge-path fno-child wall bound: `bounds.merge_gh_read_secs` (the
/// one key the pr-worktree resolve and every merge consult shares), so a
/// stalled shim is tuned in one place instead of a hardcoded 30s.
fn command_timeout(cwd: &Path) -> Duration {
    Duration::from_secs(crate::agents_config::merge_gh_read_secs(cwd))
}

/// One flow's collected output: every stdout/stderr line in write order, the
/// final receipt, and the authorized receipt's observed head when one rendered.
pub struct FlowResult {
    pub exit_code: i32,
    /// `(stderr?, line)` in write order; each line carries its own trailing
    /// newline so the operand door can replay the stream interleaving.
    pub lines: Vec<(bool, String)>,
    pub receipt: Option<Value>,
    pub observed_head: Option<Value>,
}

impl FlowResult {
    fn new() -> Self {
        Self {
            exit_code: 0,
            lines: Vec::new(),
            receipt: None,
            observed_head: None,
        }
    }
}

pub(crate) struct CommandErr {
    message: String,
    returncode: Option<i32>,
}

pub(crate) enum ClaimErr {
    /// rc 1 with `held_by_other`: a real holder, retried by polling callers.
    Held {
        holder: String,
        pid: Value,
        host: String,
    },
    /// The transport itself is down (binary missing, spawn failed): callers
    /// with a retry loop keep polling, like `CLAIM_UNAVAILABLE` on the
    /// Python side.
    Unavailable(String),
    /// An answer that failed to decode or a failed operation: terminal for
    /// the merge lock, which degrades to unlocked rather than blocking a
    /// merge on its own tooling.
    Verdict(String),
}

struct MergeLockGuard {
    key: String,
    holder: String,
}

enum LockOutcome {
    Acquired(MergeLockGuard),
    Held(String),
    Unavailable(String),
}

struct Ask<'a> {
    approved: Option<bool>,
    source: &'a str,
    require_checks: bool,
    authority: &'a str,
    accept_flake: bool,
}

/// The `fno do pr merge` operand door. `args` is post-command: `["merge", ...argv]`.
pub(crate) fn run_operand(args: &[String]) -> i32 {
    let authority = std::env::var("FNO_MERGE_AUTHORITY").unwrap_or_else(|_| "manifest".to_string());
    match args.first().map(String::as_str) {
        Some("merge") => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let mut flow = run_flow(&args[1..], &cwd, &authority, 300);
            for (err, line) in std::mem::take(&mut flow.lines) {
                if err {
                    eprint!("{}", line);
                } else {
                    print!("{}", line);
                }
            }
            flow.exit_code
        }
        Some("hold-check") => run_hold_check_operand(&args[1..]),
        // merge-result-check stays on its own command (`pr-merge-result-check`);
        // no operand door asks for it here.
        _ => {
            eprintln!(
                "fno-agents authorized-merge: not native yet; the Python leg still owns this command"
            );
            2
        }
    }
}

/// The dispatch-lane operation handler (`authorized-merge` with
/// `{"op": "merge", ...}`). The answer carries the flow receipt plus `exit`.
pub(crate) fn run_op(op: &str, payload: &Value) -> Result<Value, String> {
    match op {
        "merge" => {
            let argv: Vec<String> = payload
                .get("argv")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let authority = payload
                .get("authority")
                .and_then(Value::as_str)
                .unwrap_or("manifest");
            let cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let mut flow = run_flow(&argv, &cwd, authority, 300);
            // A flow that returns before any emit leaves the caller's last
            // receipt untouched on the Python side; carry only `exit` here.
            let mut answer = match flow.receipt.take() {
                Some(Value::Object(map)) => map,
                _ => Map::new(),
            };
            answer.insert("exit".into(), json!(flow.exit_code));
            if let Some(v) = flow.observed_head.take() {
                answer.insert("observed_head".into(), v);
            }
            Ok(Value::Object(answer))
        }
        // The check-stage door: the status cache-key op and the seams probe
        // ride the payload form; the answer carries the door's streams and
        // exit, the same shape the spawned leaf answered.
        "hold-check" => {
            let pr = payload.get("pr").and_then(Value::as_i64).unwrap_or(0);
            let cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let mut flow = hold_check_flow(pr, &cwd);
            let mut stdout = String::new();
            let mut stderr = String::new();
            for (err, line) in std::mem::take(&mut flow.lines) {
                if err {
                    stderr.push_str(&line);
                } else {
                    stdout.push_str(&line);
                }
            }
            Ok(json!({
                "exit": flow.exit_code,
                "stdout": stdout,
                "stderr": stderr,
            }))
        }
        other => Err(format!(
            "fno-agents authorized-merge: unsupported op {other}\n"
        )),
    }
}

// ---------------------------------------------------------------------------
// hold-check: the status and operator door over the shared dispatch-hold reader.
// ---------------------------------------------------------------------------

/// The `authorized-merge hold-check <pr> [--repo <path>]` operand door.
fn run_hold_check_operand(args: &[String]) -> i32 {
    let mut pr_raw: Option<&str> = None;
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--repo" && i + 1 < args.len() {
            cwd = PathBuf::from(&args[i + 1]);
            i += 2;
        } else {
            pr_raw = Some(args[i].as_str());
            i += 1;
        }
    }
    let Some(raw) = pr_raw else {
        eprintln!("hold-check: pr number required\n");
        return 1;
    };
    let Ok(pr) = raw.parse::<i64>() else {
        eprintln!("hold-check: invalid pr number: {raw}\n");
        return 1;
    };
    let mut flow = hold_check_flow(pr, &cwd);
    if flow.exit_code == 0 {
        if let Some((blocker, repo)) = visual_hold(pr, &cwd) {
            let why = format!("{}: {}", blocker.code, blocker.detail);
            if blocker.code == "visual_declined" {
                hold_refusal(&mut flow, pr, &cwd, &repo, &why);
            } else {
                flow.lines.push((true, format!("warn: {why}\n")));
            }
        }
        if let Some(why) = docs_hold(pr, &cwd) {
            flow.lines.push((true, format!("warn: {why}\n")));
        }
    }
    for (err, line) in std::mem::take(&mut flow.lines) {
        if err {
            eprint!("{line}");
        } else {
            print!("{line}");
        }
    }
    flow.exit_code
}

/// The visual-approval gate for a raw merge: `Some(detail)` when the PR
/// touches the paint surface with no clear, or when the gate cannot answer.
/// The paint list is read first, so a disarmed project spends no GitHub call.
fn visual_hold(pr: i64, cwd: &Path) -> Option<(crate::authorized_merge::Blocker, String)> {
    let pr = u64::try_from(pr).ok()?;
    let reads = crate::merge_gates::ScopedReads::for_checkout(cwd);
    let facts = match reads.info(cwd, Some(pr)) {
        Ok(facts) => facts,
        Err(e) => {
            return Some((crate::authorized_merge::Blocker::unknown("visual_head_unknown", format!(
                "visual_approval: PR {pr} head is unreadable ({e}), so the visual-approval gate cannot answer"
            )), crate::finalize::repo_identity_from_git_remote(cwd).unwrap_or_default()))
        }
    };
    crate::merge_gates::visual_approval_blocker(&reads, cwd, pr, &facts.head_sha).map(|b| {
        (
            b,
            crate::merge_grant::qualified_repo_from_pr_url(&facts.url).unwrap_or_default(),
        )
    })
}

/// The docs gate for a raw merge: `Some(detail)` when the PR touched a user
/// surface with no `Docs:` line, or when the gate cannot answer. The surface
/// list is read first, so a disarmed project spends no GitHub call.
fn docs_hold(pr: i64, cwd: &Path) -> Option<String> {
    if crate::docs_gate::user_surface_paths(cwd).is_empty() {
        return None;
    }
    let pr = u64::try_from(pr).ok()?;
    let reads = crate::merge_gates::ScopedReads::for_checkout(cwd);
    let facts = match reads.info(cwd, Some(pr)) {
        Ok(facts) => facts,
        Err(e) => {
            return Some(format!(
            "docs_surface_unknown: PR {pr} facts unreadable ({e}), so the docs gate cannot answer"
        ))
        }
    };
    crate::docs_gate::docs_gate_blocker(&reads, cwd, &facts)
        .map(|b| format!("{}: {}", b.code, b.detail))
}

/// The operator report over the same local hold read the merge and hook use.
/// An unavailable read stays distinct from a declared hold in the receipt.
pub(crate) fn hold_check_flow(pr: i64, cwd: &Path) -> FlowResult {
    let mut flow = FlowResult::new();
    let Some(number) = u64::try_from(pr).ok().filter(|number| *number > 0) else {
        flow.lines
            .push((true, "hold-check: PR number must be positive\n".into()));
        flow.exit_code = 1;
        return flow;
    };
    if std::env::var("FNO_TRACKER_BACKEND").unwrap_or_else(|_| "graph".into()) != "graph" {
        hold_clear(&mut flow, pr);
        return flow;
    }
    let verdict = crate::gate_probes::dispatch_hold_with(cwd, || {
        crate::merge_gates::ScopedReads::for_checkout(cwd).info(cwd, Some(number))
    });
    match verdict {
        crate::authorized_merge::ProbeOutcome::Clear => hold_clear(&mut flow, pr),
        crate::authorized_merge::ProbeOutcome::Refused(reason) => {
            flow.lines.push((true, format!("{reason}\n")));
            flow.exit_code = 3;
        }
        crate::authorized_merge::ProbeOutcome::Inconclusive(reason) => {
            hold_unknown(&mut flow, pr, &reason)
        }
    }
    flow
}

fn hold_clear(flow: &mut FlowResult, pr: i64) {
    flow.lines
        .push((false, format!("PR {pr}: no plan dispatch hold\n")));
    flow.exit_code = 0;
}

/// A hold read that never answered: refuse this attempt (exit 3) but leave an
/// armed auto-merge alone. No verdict was read, so nothing justifies turning
/// off a merge a green PR already earned; the next gate read decides.
fn hold_unknown(flow: &mut FlowResult, pr: i64, why: &str) {
    flow.lines.push((
        true,
        format!("{why}; PR {pr} stays armed, retry the hold read\n"),
    ));
    flow.exit_code = 3;
}

/// A look refusal retains its queue disarm through the shared scoped owner.
fn hold_refusal(flow: &mut FlowResult, pr: i64, base: &Path, repo: &str, why: &str) {
    let note = u64::try_from(pr)
        .map(|pr| crate::merge_hold::disarm_automerge(base, pr, repo))
        .unwrap_or_else(|_| "not issued: PR number is invalid".into());
    flow.lines.push((
        true,
        format!("hold: auto-merge disarm for PR {pr}: {note}\n"),
    ));
    flow.lines
        .push((true, format!("{why}; refusing to assume unheld\n")));
    flow.exit_code = 3;
}

pub fn run_flow(argv: &[String], cwd: &Path, authority: &str, timeout_s: u64) -> FlowResult {
    let mut flow = FlowResult::new();
    let mut repo = cwd.to_path_buf();
    let mut pr_raw: Option<String> = None;
    let mut accept_flake = false;
    for arg in argv {
        if arg.starts_with("--invoker=") {
            continue;
        }
        if arg == "--accept-flake" {
            accept_flake = true;
            continue;
        }
        match arg.chars().next() {
            Some(c) if c.is_numeric() => pr_raw = Some(arg.clone()),
            _ => {
                flow.lines
                    .push((true, format!("Error: unknown arg '{arg}'\n")));
                flow.exit_code = 1;
                return flow;
            }
        }
    }
    let Some(raw) = pr_raw.as_deref() else {
        flow.lines.push((true, "pr_number required\n".into()));
        flow.exit_code = 1;
        return flow;
    };
    if !is_pr_number(raw) {
        emit(
            &mut flow,
            0,
            "failed",
            &format!("invalid pr number: {raw}"),
            "none",
            true,
            None,
        );
        flow.exit_code = 1;
        return flow;
    }
    let pr: i64 = raw.parse().expect("validated pr digits");

    match resolve_pr_worktree(pr, &repo) {
        Ok(wt) if !wt.as_os_str().is_empty() => repo = wt,
        Ok(_) => {}
        Err(message) => {
            emit(&mut flow, pr, "held", &message, "none", true, None);
            flow.exit_code = 2;
            return flow;
        }
    }

    // Incarnation fence: silent no-op at this stage. The fence stage adds the
    // claim check; today absence fails open, exactly like a crashed Python
    // check does.

    // Posture: which auto-merge posture to hand the owner. The durable-grant
    // arm replaces the manifest for a parked worker; everything else rides
    // the manifest fields as written.
    let approved_field = manifest_field(&repo, "auto_merge_approved").unwrap_or_default();
    let mut posture_source = manifest_field(&repo, "auto_merge_source")
        .unwrap_or_default()
        .trim()
        .to_string();
    let posture_approved: Option<bool>;
    if authority == "durable_grant" {
        let (state, reason) = resolve_durable_grant(pr, &repo);
        if state != "granted" {
            let (word, err) = match state.as_str() {
                "refused" | "absent" => ("skipped", false),
                "held" => ("held", false),
                _ => ("blocked", true),
            };
            emit(
                &mut flow,
                pr,
                word,
                &format!("durable-grant merge not executable: {reason}"),
                "none",
                err,
                None,
            );
            flow.exit_code = 2;
            return flow;
        }
        // The resolver already said eligible and re-checked the standing
        // config itself; naming its own source keeps it out of the
        // manifest-posture arm so the owner still holds it to the live
        // switch and the floor.
        posture_approved = Some(true);
        posture_source = "durable-grant".to_string();
    } else if approved_field.is_empty() {
        // No manifest, or a manifest predating the field: the live config
        // decides on its own. A manual merge outside a target session is
        // legitimate.
        posture_approved = None;
    } else {
        posture_approved = Some(approved_true(&approved_field));
    }

    // (2) gh must be installed.
    if !gh_on_path() {
        emit(
            &mut flow,
            pr,
            "failed",
            "gh CLI not installed",
            "none",
            true,
            None,
        );
        flow.exit_code = 127;
        return flow;
    }

    let require_checks = crate::agents_config::auto_merge_require_checks_pass(&repo);

    // The owner captures and pins fresh PR facts. This preliminary rerun
    // evidence does not provide or withhold the effect's head pin.
    let flake: Option<Value> = if require_checks {
        match command_call(
            "authorized-merge",
            &json!({"op": "status-rerun", "cwd": repo.to_string_lossy(), "pr": pr, "sha": ""}),
            FLAKE_PROBE_TIMEOUT,
        ) {
            Ok(v) => Some(v),
            Err(e) => {
                flow.lines.push((
                    true,
                    format!(
                        "pr-merge: rerun-recovery probe unavailable ({}); merging without the flake hold\n",
                        e.message
                    ),
                ));
                None
            }
        }
    } else {
        None
    };

    // (2c) Merge serialization: builds run parallel, merges run one at a
    // time. The authorized-merge gate reads CI freshness under the lock, so
    // a peer merge cannot land between that read and this merge.
    let lock = acquire_merge_lock(&repo, pr);
    match &lock {
        LockOutcome::Unavailable(exc) => {
            flow.lines.push((
                true,
                format!("pr-merge: merge lock unavailable ({exc}); proceeding\n"),
            ));
        }
        LockOutcome::Held(reason) => {
            emit(&mut flow, pr, "held", reason, "none", false, None);
            flow.exit_code = 2;
            return flow;
        }
        // Binding the guard counts as the field's read; its Drop in `lock`
        // releases the claim only after the merge below finishes.
        LockOutcome::Acquired(_guard) => {}
    }
    let code = do_merge(
        &mut flow,
        pr,
        &repo,
        authority,
        accept_flake,
        timeout_s,
        flake.as_ref(),
        posture_approved,
        &posture_source,
        require_checks,
    );
    drop(lock);
    flow.exit_code = code;
    flow
}

#[allow(clippy::too_many_arguments)]
fn do_merge(
    flow: &mut FlowResult,
    pr: i64,
    repo: &Path,
    authority: &str,
    accept_flake: bool,
    timeout_s: u64,
    flake: Option<&Value>,
    posture_approved: Option<bool>,
    posture_source: &str,
    require_checks: bool,
) -> i32 {
    let strategy = crate::agents_config::auto_merge_strategy(repo);
    let ask = Ask {
        approved: posture_approved,
        source: posture_source,
        require_checks,
        authority,
        accept_flake,
    };

    // Authorize BEFORE publishing anything. The coverage status greens the
    // head for the web button, which enforces only the coverage context, so
    // stamping it ahead of a refusal would open the door this flow is about
    // to close.
    let decision = authorized_merge(pr, repo, &ask, true, timeout_s);
    if decision.get("outcome").and_then(Value::as_str) != Some("authorized") {
        return emit_authorized_outcome(flow, pr, &decision, &strategy);
    }

    // The flake hold: a rerun-recovered green is not a clean green. The probe
    // ran before the lock; this is only the receipt. It fires on an accepted
    // rerun recovery and lands with the follow-ups stage (event + note).
    if accept_flake
        && flake
            .and_then(|f| f.get("recovered"))
            .and_then(Value::as_bool)
            == Some(true)
    {
        // follow-ups stage: the merge-flake-accepted event and the
        // "flake override accepted: <checks>" stderr note.
    }

    // Server-visible receipt of the verdict the gate acted on, posted to the
    // head GitHub names for the PR. The head resolution stays the staged command
    // hop the captured goldens record - the fold parity reads it - and the
    // POST rides the in-process publisher. Best-effort: the durable verdict
    // is the event row, and a failed POST leaves the status absent, which the
    // ruleset reads as not-passing.
    let publish_head = match command_call(
        "authorized-merge",
        &json!({"op": "pr-head", "pr": pr, "cwd": repo.to_string_lossy()}),
        command_timeout(&repo),
    ) {
        Ok(v) => v
            .get("head")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    };
    let (posted, note) = if publish_head.is_empty() {
        (false, "no PR head to publish a status on".to_string())
    } else {
        match u64::try_from(pr) {
            Ok(pr_u64) => {
                crate::loopcheck::coverage_gate::publish::publish(repo, pr_u64, Some(&publish_head))
            }
            Err(_) => (false, format!("pr number {pr} is not a u64")),
        }
    };
    if !posted {
        flow.lines.push((
            true,
            format!("pr-merge: coverage status not posted: {note}\n"),
        ));
    }

    // The effect re-runs the whole decision, so the window the receipt opened
    // is closed again by the same guards, and the merge stays pinned to the
    // head those guards read. The second pass is the price of that window,
    // not an oversight: do not fold it into one pass.
    let receipt = authorized_merge(pr, repo, &ask, false, timeout_s);
    if receipt.get("outcome").and_then(Value::as_str) == Some("merged") {
        // follow-ups stage: the confirmed-merge finish (origin fetch, note,
        // branch cleanup, node close) lands there. At this stage the merged
        // receipt renders through the same table as every other outcome.
    }
    emit_authorized_outcome(flow, pr, &receipt, &strategy)
}

/// Ask the one authorized-merge operation. The decision lives in
/// `authorized_merge.rs`; this module is the transport and the renderer. An
/// unavailable binary is reported as `unknown`, never as a clear answer: a
/// merge whose authorization could not be read has not been authorized.
fn authorized_merge(pr: i64, repo: &Path, ask: &Ask, decide_only: bool, timeout_s: u64) -> Value {
    let mut payload = Map::new();
    payload.insert("cwd".into(), json!(repo.to_string_lossy()));
    payload.insert("pr".into(), json!(pr));
    payload.insert("effect".into(), json!("merge"));
    payload.insert("auto_merge_source".into(), json!(ask.source));
    payload.insert("require_checks".into(), json!(ask.require_checks));
    payload.insert("decide_only".into(), json!(decide_only));
    payload.insert("authority".into(), json!(ask.authority));
    payload.insert("accept_flake".into(), json!(ask.accept_flake));
    if let Some(approved) = ask.approved {
        payload.insert("approved".into(), json!(approved));
    }
    match command_call(
        "authorized-merge",
        &Value::Object(payload),
        crate::merge_architect::operation_budget(repo, timeout_s),
    ) {
        Ok(v) => v,
        Err(e) => json!({
            "outcome": "unknown",
            "detail": format!(
                "the authorized-merge owner could not be reached ({}); refusing to merge on an unread authorization",
                e.message
            ),
        }),
    }
}

/// Receipt word -> (emit word, exit code, stderr?). `failed` is the only arm
/// that spends the failure budget; every other refusal is the retry-or-
/// escalate 2 the merge lane already speaks.
fn outcome_emit(outcome: &str) -> (&'static str, i32, bool) {
    match outcome {
        "merged" => ("merged", 0, false),
        "armed" | "authorized" | "refused" => ("skipped", 2, false),
        "held" | "head_changed" | "unknown" => ("held", 2, false),
        "failed" => ("failed", 1, true),
        _ => ("held", 2, false),
    }
}

/// Render one authorized-merge receipt onto this command's surface.
fn emit_authorized_outcome(flow: &mut FlowResult, pr: i64, receipt: &Value, strategy: &str) -> i32 {
    let outcome = match receipt.get("outcome") {
        None | Some(Value::Null) => "unknown".to_string(),
        Some(v) => py_str(v),
    }
    .pipe_if_empty("unknown");
    if let Some(warnings) = receipt.get("warnings").and_then(Value::as_array) {
        for warning in warnings {
            if let (Some(code), Some(detail)) = (
                warning.get("code").and_then(Value::as_str),
                warning.get("detail").and_then(Value::as_str),
            ) {
                flow.lines.push((true, format!("warn: {code}: {detail}\n")));
            }
        }
    }
    let (word, code, err) = outcome_emit(&outcome);
    let detail = match receipt.get("detail") {
        None | Some(Value::Null) => "no detail".to_string(),
        Some(v) => py_str(v),
    }
    .pipe_if_empty("no detail");
    emit(
        flow,
        pr,
        word,
        &format!("{outcome}: {detail}"),
        strategy,
        err,
        receipt.get("warnings").filter(|value| value.is_array()),
    );
    flow.observed_head = receipt.get("observed_head").cloned();
    // code == 1: the failed arm also syncs the graph merge row; lands with
    // the follow-ups stage. No captured golden at this stage reaches it.
    code
}

trait PipeIfEmpty {
    fn pipe_if_empty(self, fallback: &str) -> String;
}

impl PipeIfEmpty for String {
    fn pipe_if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn resolve_durable_grant(pr: i64, repo: &Path) -> (String, String) {
    const UNKNOWN: &str = "unknown";
    let payload = json!({"op": "grant-verdict", "pr": pr, "cwd": repo.to_string_lossy()});
    let out = match command_call("authorized-merge", &payload, command_timeout(repo)) {
        Ok(v) => v,
        Err(e) => {
            return (
                UNKNOWN.to_string(),
                format!(
                    "the durable-grant resolver could not be reached ({}); an unread grant never grants",
                    e.message
                ),
            )
        }
    };
    let state = match out.get("state") {
        None | Some(Value::Null) => UNKNOWN.to_string(),
        Some(v) => py_str(v),
    }
    .pipe_if_empty(UNKNOWN);
    let reason = match out.get("reason") {
        None | Some(Value::Null) => String::new(),
        Some(v) => py_str(v),
    }
    .pipe_if_empty("");
    (state, reason)
}

fn resolve_pr_worktree(pr: i64, repo: &Path) -> Result<PathBuf, String> {
    let payload = json!({"cwd": repo.to_string_lossy(), "pr": pr});
    match command_call("pr-worktree", &payload, command_timeout(repo)) {
        Ok(v) => match v.get("worktree") {
            Some(w) => Ok(PathBuf::from(py_str(w))),
            // A zero-exit answer without the key surfaces as the KeyError
            // text the Python leg would raise.
            None => Err("'worktree'".to_string()),
        },
        Err(e) => {
            if e.returncode == Some(3) && e.message.contains("no local worktree") {
                Ok(PathBuf::new())
            } else {
                Err(e.message)
            }
        }
    }
}

fn emit(
    flow: &mut FlowResult,
    pr: i64,
    outcome: &str,
    reason: &str,
    strategy: &str,
    err: bool,
    warnings: Option<&Value>,
) {
    let mut row = Map::new();
    row.insert("pr".into(), json!(pr));
    row.insert("outcome".into(), json!(outcome));
    row.insert("reason".into(), json!(reason));
    row.insert("strategy".into(), json!(strategy));
    if let Some(warnings) = warnings {
        row.insert("warnings".into(), warnings.clone());
    }
    flow.receipt = Some(Value::Object(row.clone()));
    flow.lines.push((err, format!("{}\n", Value::Object(row))));
}

fn is_pr_number(raw: &str) -> bool {
    let mut chars = raw.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_digit() && c != '0')
        && chars.all(|c| c.is_ascii_digit())
}

/// Python truthy spellings for `auto_merge_approved`. Mirrored, not shared:
/// the finalize gate accepts only the literal `true`, so a widening here must
/// not silently move that gate.
fn approved_true(value: &str) -> bool {
    matches!(value.trim().to_lowercase().as_str(), "true" | "yes" | "1")
}

fn manifest_field(repo: &Path, field: &str) -> Option<String> {
    let path = crate::state_path::resolve("target-state", repo)?;
    let content = fs::read_to_string(path).ok()?;
    crate::loopcheck::scan_manifest_field(&content, field)
}

fn resolve_agents_bin() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_AGENTS_BIN").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| hop_client_bin(&exe))
}

/// The hop target for a caller whose own exe is `exe`: itself, unless that exe
/// IS the resident daemon binary, in which case the client sibling beside it.
/// `command_call` and `native_claim` execute this path.
/// Returning the daemon path here once booted a FULL RESIDENT DAEMON wearing
/// the hop's argv (`fno-agents-daemon pr-worktree`) that went on to win the
/// freed singleton and serve as the control plane (observed 2026-11-03); the
/// daemon entrypoint now refuses such argv outright, so a daemon path would
/// turn every hop into that refusal. A missing sibling keeps None, so every
/// hop takes its missing-binary refusal naming FNO_AGENTS_BIN instead.
pub(crate) fn hop_client_bin(exe: &Path) -> Option<PathBuf> {
    if !is_daemon_exe(exe) {
        return Some(exe.to_path_buf());
    }
    let exe_name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let sibling_name = if exe_name.ends_with(".exe") {
        "fno-agents.exe"
    } else {
        "fno-agents"
    };
    let sibling = exe.with_file_name(sibling_name);
    sibling.is_file().then_some(sibling)
}

/// The `.exe` arm keeps a Windows build classifying the same way; the plain
/// name is the only spelling this repo's builds produce today.
fn is_daemon_exe(exe: &Path) -> bool {
    matches!(
        exe.file_name().and_then(|n| n.to_str()),
        Some("fno-agents-daemon" | "fno-agents-daemon.exe")
    )
}

/// One command hop into the fno-agents binary: `[bin, command]` with the JSON
/// payload on stdin. Stdout must be one JSON value; a non-zero exit or an
/// unreadable answer is a `CommandErr` naming the binary, never a panic.
fn command_call(command: &str, payload: &Value, timeout: Duration) -> Result<Value, CommandErr> {
    let bin = resolve_agents_bin().ok_or_else(|| CommandErr {
        message: BIN_MISSING.to_string(),
        returncode: None,
    })?;
    let wire = payload.to_string();
    let mut child = Command::new(&bin)
        .arg(command)
        .env(crate::authorized_merge::step_timer::TRACE_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CommandErr {
            message: format!("fno-agents {command} failed: {e}"),
            returncode: None,
        })?;
    {
        // The claim stub lanes never read stdin, so an early EPIPE here is
        // the normal case, not an error.
        let mut stdin = child.stdin.take().expect("piped stdin");
        let _ = stdin.write_all(wire.as_bytes());
    }
    // Drain both pipes while the child runs: a child that fills one pipe's
    // buffer blocks on the write and spends the whole bound doing nothing.
    // stderr lands in a shared buffer so a timeout reads what arrived without
    // waiting on a grandchild that still holds the pipe.
    let stdout_reader = {
        let pipe = child.stdout.take();
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_reader = {
        let pipe = child.stderr.take();
        let buf = std::sync::Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let Some(pipe) = pipe else { return };
            for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
                if let Ok(mut text) = buf.lock() {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        })
    };
    let stderr_so_far = || stderr_buf.lock().map(|t| t.clone()).unwrap_or_default();
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout_reader.join().unwrap_or_default();
                let _ = stderr_reader.join();
                let stderr = trace_free(&stderr_so_far());
                let rc = status.code().unwrap_or(-1);
                if rc != 0 {
                    return Err(CommandErr {
                        message: format!(
                            "fno-agents {command} exited {rc}: {}",
                            stderr.trim().chars().take(200).collect::<String>()
                        ),
                        returncode: Some(rc),
                    });
                }
                return serde_json::from_str(stdout.trim()).map_err(|e| CommandErr {
                    message: format!("fno-agents {command} bad output: {e}"),
                    returncode: None,
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let at = crate::authorized_merge::step_timer::unfinished(&stderr_so_far())
                        .map(|step| format!(" inside step {step}"))
                        .unwrap_or_default();
                    return Err(CommandErr {
                        message: format!(
                            "fno-agents {command} failed: timed out after {}s{at}",
                            timeout.as_secs()
                        ),
                        returncode: None,
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(CommandErr {
                    message: format!("fno-agents {command} failed: {e}"),
                    returncode: None,
                })
            }
        }
    }
}

/// The child's stderr without the step-trace lines, so a failure detail
/// capped at 200 chars still carries the child's own message.
fn trace_free(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.starts_with("authorized-merge: step "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `shutil.which("gh")`: a PATH scan for an executable file named `gh`.
fn gh_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join("gh");
        if let Ok(meta) = fs::metadata(&candidate) {
            if meta.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o111 != 0 {
                        return true;
                    }
                    continue;
                }
                #[cfg(not(unix))]
                return true;
            }
        }
    }
    false
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `urllib.parse.quote(key, safe="")`: unreserved bytes pass, everything
/// else becomes uppercase `%XX`.
fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn configured_claim_root() -> Option<PathBuf> {
    std::env::var_os("FNO_CLAIMS_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Mirror of `claims.io.claims_dir(None)` through the shared resolver: the
/// env root wins and nests `.fno/claims` beneath it; without one the repo
/// space holds the claims directory directly (no nested `.fno` segment).
/// The merge-lock key carries no global-id prefix, so the resolver never
/// routes it to the machine-wide root.
fn claim_path(cwd: &Path, key: &str) -> PathBuf {
    crate::claims_root::claims_dir_in(
        key,
        None,
        || Ok(cwd.to_path_buf()),
        std::env::var_os("FNO_CLAIMS_ROOT"),
        None,
    )
    .expect("cwd is in hand, the space fallback cannot fail")
    .join(format!("{}.lock", encode_key(key)))
}

fn claim_root_flags() -> Vec<String> {
    match configured_claim_root() {
        Some(root) => vec!["--root".to_string(), root.to_string_lossy().to_string()],
        None => Vec::new(),
    }
}

/// One claim operation through the binary: `[bin, claim, operation]` (+ key,
/// flags, `--json`), JSON on stdout. Empty stdout is an empty object, a
/// corrupted status row is an answer, and rc 1 with `held_by_other` is the
/// typed held error the merge lock polls on.
fn native_claim(operation: &str, key: Option<&str>, flags: &[String]) -> Result<Value, ClaimErr> {
    let bin = resolve_agents_bin().ok_or_else(|| {
        ClaimErr::Unavailable(
            "fno-agents claim unavailable: set FNO_AGENTS_BIN or reinstall fno".into(),
        )
    })?;
    let mut argv: Vec<String> = vec!["claim".into(), operation.to_string()];
    if let Some(k) = key {
        // The root op's key IS the question and an empty key is legal input;
        // every other operation skips an empty key. The merge command never
        // calls root.
        if !k.is_empty() || operation == "root" {
            argv.push(k.to_string());
        }
    }
    argv.extend(flags.iter().cloned());
    argv.push("--json".into());
    let output = Command::new(&bin)
        .args(&argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ClaimErr::Unavailable(format!("fno-agents claim could not run: {e}")))?;
    let rc = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let payload: Value = if stdout.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        match serde_json::from_str(stdout.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Err(ClaimErr::Verdict(format!(
                    "fno-agents claim returned invalid JSON: {e}"
                )))
            }
        }
    };
    if rc == 1 && payload.get("outcome").and_then(Value::as_str) == Some("held_by_other") {
        return Err(ClaimErr::Held {
            holder: payload
                .get("holder")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            pid: payload.get("pid").cloned().unwrap_or(Value::Null),
            host: payload
                .get("host")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown")
                .to_string(),
        });
    }
    // A corrupted row is an answer, not a failure: status prints its verdict
    // and exits 2 so shell callers notice.
    if operation == "status" && payload.get("state").and_then(Value::as_str) == Some("corrupted") {
        return Ok(payload);
    }
    if rc != 0 {
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit {rc}")
        };
        return Err(ClaimErr::Verdict(format!(
            "fno-agents claim {operation} failed: {detail}"
        )));
    }
    if operation == "list" && payload.is_array() {
        return Ok(json!({ "rows": payload }));
    }
    if !payload.is_object() {
        return Err(ClaimErr::Verdict(
            "fno-agents claim returned a non-object JSON value".into(),
        ));
    }
    Ok(payload)
}

fn acquire_flags(holder: &str) -> Vec<String> {
    // acquire_claim(key, holder, reason="serialized PR merge (LD#9)"): no
    // ttl, no metadata, no host/harness; pid defaults to this process.
    let mut flags = vec![
        "--holder".to_string(),
        holder.to_string(),
        "--reason".to_string(),
        "serialized PR merge (LD#9)".to_string(),
        "--pid".to_string(),
        std::process::id().to_string(),
    ];
    flags.extend(claim_root_flags());
    flags
}

/// The merge lock's repo-wide key: one `merge:<canonical-root>` claim per
/// project, so every worktree lane contends on the SAME lock.
fn lock_repo_root(cwd: &Path) -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_REPO_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    match crate::paths::canonical_repo_root(cwd) {
        Some(p) => p,
        None => crate::paths::worktree_repo_root(cwd),
    }
}

/// Best-effort claim age for the held report; any failure just drops the
/// ", held Ns" suffix from the reason.
fn read_lock_age(cwd: &Path, key: &str) -> Option<i64> {
    let path = claim_path(cwd, key);
    let payload = native_claim("read-path", Some(&path.to_string_lossy()), &[]).ok()?;
    let acquired_at = payload.get("acquired_at").and_then(Value::as_i64)?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(0.max((now_ms - acquired_at) / 1000))
}

impl Drop for MergeLockGuard {
    fn drop(&mut self) {
        // release_claim strict=False; pid-liveness frees the lock anyway, so
        // errors are ignored.
        let mut flags = vec![
            "--holder".to_string(),
            self.holder.clone(),
            "--with-claim".to_string(),
        ];
        flags.extend(claim_root_flags());
        let _ = native_claim("release", Some(&self.key), &flags);
    }
}

fn acquire_merge_lock(cwd: &Path, pr: i64) -> LockOutcome {
    let key = format!("merge:{}", lock_repo_root(cwd).display());
    let holder = format!("pr-merge:pr{pr}:{}", std::process::id());
    let deadline = Instant::now() + Duration::from_secs(MERGE_LOCK_WAIT_S);
    let mut held_detail: Option<(String, Value, String)> = None;
    loop {
        match native_claim("acquire", Some(&key), &acquire_flags(&holder)) {
            Ok(_) => return LockOutcome::Acquired(MergeLockGuard { key, holder }),
            Err(ClaimErr::Held {
                holder: h,
                pid,
                host,
            }) => held_detail = Some((h, pid, host)),
            // Stale claims are stolen by the claims layer, so a held report
            // always names a holder live at the last poll. A decode failure
            // degrades to unlocked: the lock is coordination, GitHub stays
            // the merge authority, and our own tooling failing must never
            // block a merge.
            Err(ClaimErr::Verdict(message)) => return LockOutcome::Unavailable(message),
            Err(ClaimErr::Unavailable(message)) => {
                // The transport being down still waits out the window: at
                // the exact moment two mergers race hardest is when the
                // serialization matters.
                let _ = message;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_secs(MERGE_LOCK_POLL_S));
    }
    let reason = match held_detail {
        Some((h, _pid, _host)) => {
            let mut age_part = String::new();
            if let Some(age) = read_lock_age(cwd, &key) {
                if age > 0 {
                    age_part = format!(", held {age}s");
                }
            }
            format!(
                "merge serialized: held by {h} (live at last poll{age_part}; waited {MERGE_LOCK_WAIT_S}s); retry"
            )
        }
        None => "merge serialized: another merge holds the lock; retry".to_string(),
    };
    LockOutcome::Held(reason)
}

#[cfg(test)]
mod retry_read_tests {
    use super::*;

    #[test]
    fn an_unknown_read_refuses_without_disarming() {
        let mut flow = FlowResult::new();
        hold_unknown(&mut flow, 7, "dispatch-hold-unknown:x-1: read failed");
        assert_eq!(flow.exit_code, 3);
        assert!(flow
            .lines
            .iter()
            .all(|(_, l)| !l.contains("disabled an armed")));
        assert!(flow.lines[0].1.contains("stays armed"));
    }
}

#[cfg(test)]
mod hop_client_bin_tests {
    use super::{hop_client_bin, is_daemon_exe};

    fn touch(path: &std::path::Path) {
        std::fs::write(path, b"").expect("test sibling writes");
    }

    #[test]
    fn a_daemon_caller_hops_to_its_client_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = dir.path().join("fno-agents-daemon");
        touch(&daemon);
        let client = dir.path().join("fno-agents");
        touch(&client);
        assert_eq!(hop_client_bin(&daemon), Some(client));
        assert!(is_daemon_exe(&daemon));
    }

    #[test]
    fn a_daemon_caller_without_the_client_sibling_hops_nowhere() {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = dir.path().join("fno-agents-daemon");
        touch(&daemon);
        assert_eq!(hop_client_bin(&daemon), None);
    }

    #[test]
    fn a_client_caller_hops_to_itself() {
        let client = std::path::Path::new("/opt/x/bin/fno-agents");
        assert_eq!(hop_client_bin(client), Some(client.to_path_buf()));
        assert!(!is_daemon_exe(client));
    }

    #[test]
    fn the_windows_exe_spelling_hops_to_the_exe_client_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let daemon = dir.path().join("fno-agents-daemon.exe");
        touch(&daemon);
        let client = dir.path().join("fno-agents.exe");
        touch(&client);
        assert_eq!(hop_client_bin(&daemon), Some(client));
    }
}

#[cfg(test)]
mod warning_receipt_tests {
    use super::*;

    #[test]
    fn a_merge_warning_reaches_cli_streams_without_changing_the_result() {
        let mut flow = FlowResult::new();
        let receipt = json!({"outcome":"merged", "detail":"abc123", "warnings":[
            {"code":"docs_line_missing", "detail":"doc evidence"}
        ]});
        assert_eq!(emit_authorized_outcome(&mut flow, 7, &receipt, "merge"), 0);
        assert_eq!(
            flow.receipt.as_ref().unwrap().get("outcome").unwrap(),
            "merged"
        );
        assert_eq!(
            flow.receipt.as_ref().unwrap().get("warnings"),
            receipt.get("warnings")
        );
        let stdout: Value = flow
            .lines
            .iter()
            .filter(|(err, _)| !err)
            .find_map(|(_, line)| serde_json::from_str(line).ok())
            .expect("actual stdout JSON");
        assert_eq!(stdout.get("warnings"), receipt.get("warnings"));
        assert_eq!(stdout.get("outcome").unwrap(), "merged");
        assert!(flow
            .lines
            .iter()
            .any(|(err, line)| *err && line == "warn: docs_line_missing: doc evidence\n"));
    }
}
