//! Read-confirmed recovery for unfinished workers. Runs under the settle arm's
//! existing cadence and one-in-flight gate; a quiet transcript is not death.

use crate::harness::Harness;
use crate::paths::AgentsHome;
use crate::state::RegistryEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SENDER: &str = "fno/quiet-recovery";
const TAIL_BYTES: u64 = 256 * 1024;

/// A parked session stays shielded for one quiet interval past its watch's
/// ring: the moment its alarm last went off. The ring is the bound itself,
/// or an earlier wake row's stamp when the watch woke ahead of its bound.
/// The worker then needs that interval to wake, act and re-arm (or proceed)
/// before its silence reads as stuck. The shield still requires the watch
/// to be the session's current one, so a superseded watch releases the seat
/// at once; the wake row is not superseding evidence, so it never lifts the
/// shield, and a re-arm supersedes the row inside `settle_watches`. Without
/// the linger, every watch bound evaluated the ladder against a pre-watch
/// activity stamp, flagged the healthy parked worker stuck, and each failed
/// resume salvaged a recovery branch.
const WATCH_LINGER_MS: i64 = 900 * 1000;

/// An unreadable reset stamp holds one vendor window: the shared-account 429
/// names a 5-hour usage pool, so the error's own stamp plus five hours is a
/// conservative floor. The hold can outlast the true reset, never end early.
const QUOTA_UNKNOWN_RESET_HOLD_S: i64 = 5 * 3600;

pub(super) fn watch_shielded(
    watch: &crate::watch_expiry::Watch,
    evidence: &[crate::watch_expiry::Evidence],
    now_ms: i64,
) -> bool {
    let ring = wake_ring(watch, evidence)
        .unwrap_or(watch.expires_at_ms)
        .min(watch.expires_at_ms);
    now_ms < ring + WATCH_LINGER_MS && crate::watch_expiry::is_current_watch(watch, evidence)
}

/// The newest wake row stamped for this watch, when one exists. The same
/// identity `woken_already` matches: session plus `watch_event_id`.
fn wake_ring(
    watch: &crate::watch_expiry::Watch,
    evidence: &[crate::watch_expiry::Evidence],
) -> Option<i64> {
    evidence
        .iter()
        .filter(|row| {
            row.session_id.as_deref() == Some(watch.session_id.as_str())
                && row.kind == crate::watch_expiry::WAKE_EVENT
                && row.data.get("watch_event_id").and_then(Value::as_str)
                    == Some(watch.event_id.as_str())
        })
        .map(|row| row.ts_ms)
        .max()
}

#[derive(Default, Serialize, Deserialize)]
struct Episode {
    activity: i64,
    read_stage: u8,
    pending: Option<(String, i64, u8)>,
    escalated: bool,
    #[serde(default)]
    recovered: bool,
    #[serde(default)]
    recovery_reason: Option<String>,
    /// The future reset of the quota 429 the tail's newest decisive row
    /// carries, when it does. While it stands ahead of now, every ladder
    /// action holds: a nudge, resume or help mail to a quota-blocked worker
    /// is one more rejected request against the shared pool.
    #[serde(default)]
    quota_reset_at: Option<i64>,
}

/// A tool call the transcript shows open: a `tool_use` with no `tool_result`
/// yet. A foreground wait (a REST poll loop inside one Bash call) leaves
/// exactly this at the tail while it runs, so the worker is busy, not quiet.
/// Fields: call id, start in unix seconds, timeout in milliseconds.
struct OpenCall {
    id: String,
    started_s: i64,
    timeout_ms: i64,
}

/// The harness default for a tool call that names no timeout (Claude Bash).
const DEFAULT_CALL_TIMEOUT_MS: i64 = 120_000;

#[derive(Default)]
pub(super) struct Tail {
    pub(super) activity: i64,
    started_at: Option<i64>,
    failed: bool,
    read: bool,
    pub(super) touched: i64,
    open_calls: Vec<OpenCall>,
}

impl Tail {
    /// True while an open tool call has not yet passed its own timeout. The
    /// call's timeout is its bound: past it, the call is stale, and the
    /// ladder reads the worker quiet again.
    pub(super) fn call_open(&self, now: i64) -> bool {
        self.open_calls
            .iter()
            .any(|call| now * 1000 < call.started_s * 1000 + call.timeout_ms)
    }
}

/// The timeout a tool call declares, in milliseconds. Claude `input.timeout`
/// and Codex `timeout_ms` both carry it; a call that names none gets the
/// harness default.
fn declared_timeout_ms(input: Option<&Value>) -> i64 {
    input
        .and_then(|v| {
            v.get("timeout")
                .or_else(|| v.get("timeout_ms"))
                .and_then(Value::as_i64)
        })
        .filter(|ms| *ms > 0)
        .unwrap_or(DEFAULT_CALL_TIMEOUT_MS)
}

/// Fold one transcript row into the set of open tool calls. Claude carries
/// calls as `message.content` items (`tool_use` opens, `tool_result` closes).
/// Codex carries them as payload items (`function_call` opens,
/// `function_call_output` closes), with arguments as a JSON string.
fn fold_tool_calls(
    row: &Value,
    payload: &Value,
    event: &str,
    at: Option<i64>,
    open: &mut Vec<OpenCall>,
) {
    if let Some(items) = row.pointer("/message/content").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if let (Some(id), Some(started_s)) =
                        (item.get("id").and_then(Value::as_str), at)
                    {
                        open.push(OpenCall {
                            id: id.to_string(),
                            started_s,
                            timeout_ms: declared_timeout_ms(item.get("input")),
                        });
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = item.get("tool_use_id").and_then(Value::as_str) {
                        open.retain(|call| call.id != id);
                    }
                }
                _ => {}
            }
        }
    }
    match event {
        "function_call" => {
            if let (Some(id), Some(started_s)) =
                (payload.get("call_id").and_then(Value::as_str), at)
            {
                let args = payload
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str::<Value>(text).ok());
                open.push(OpenCall {
                    id: id.to_string(),
                    started_s,
                    timeout_ms: declared_timeout_ms(args.as_ref()),
                });
            }
        }
        "function_call_output" => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                open.retain(|call| call.id != id);
            }
        }
        _ => {}
    }
}

pub(super) fn tail(path: &Path, marker: Option<&str>) -> Option<Tail> {
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let size = meta.len();
    let touched = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Tail {
        touched,
        ..Default::default()
    };
    for line in text.lines().skip(usize::from(size > TAIL_BYTES)) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if marker.is_some_and(|marker| line.contains(marker)) {
            out.read = true;
            continue;
        }
        let payload = row.get("payload").unwrap_or(&row);
        let kind = row.get("type").and_then(Value::as_str).unwrap_or("");
        let event = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let timestamp = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp());
        fold_tool_calls(&row, payload, event, timestamp, &mut out.open_calls);
        if event == "task_started" {
            out.failed = false;
            out.started_at = timestamp.or(out.started_at);
        }
        if event == "task_complete" {
            out.failed = payload.get("error").is_some_and(|e| !e.is_null());
        }
        if matches!(kind, "response_item" | "assistant" | "user" | "tool")
            || matches!(event, "item_completed" | "task_started" | "task_complete")
        {
            if event != "task_complete" {
                out.failed = false;
            }
            if let Some(at) = timestamp {
                out.activity = out.activity.max(at);
            }
        }
    }
    (out.activity > 0 || out.started_at.is_some() || out.read || out.failed || out.touched > 0)
        .then_some(out)
}

/// The future reset a quota-capped transcript tail names, when it does.
/// Real work at the tail, a reset that already passed, and an unreadable
/// tail all read `None`: the hold must never be older news than the worker.
/// The verdict scan is the provider-cap sweep's own, so quiet recovery and
/// the lane readout cannot disagree on what "capped" means. A naive stamp
/// no zone resolves holds one window from the error's stamp.
fn quota_reset_ahead(path: Option<&Path>, codex: bool, now: i64) -> Option<i64> {
    let path = path?;
    // The window-miss unknown pairs with a None excerpt, so dropping the
    // third field reads the same not-blocked answer.
    let (ts, excerpt, _unknown) = if codex {
        crate::provider_cap::codex_capped_tail(path)
    } else {
        crate::provider_cap::capped_tail(path)
    };
    let excerpt = excerpt?;
    let reset =
        crate::provider_cap::reset_epoch_from_excerpt(&excerpt, None, now).or_else(|| {
            ts.as_deref()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.timestamp() + QUOTA_UNKNOWN_RESET_HOLD_S)
        })?;
    (reset > now).then_some(reset)
}

/// The newest `help_route` row per emitting session, folded from the
/// global journal once per tick: the same rows the help-router sweep acts
/// on, so the shield and the sweep cannot disagree on the newest truth.
/// `(ts, route kind)` per session id (`run`).
fn newest_help_routes(global: &Path) -> std::collections::HashMap<String, (i64, String)> {
    fold_newest_help_routes(crate::event_store::journal_text(global, &["help_route"]).lines())
}

/// One `help_route` journal row as the fold reads it: the emitting session,
/// its stamp, and the route kind. Unknown fields are ignored; a missing
/// stamp or data block reads empty, the same tolerance the journal's
/// mixed lines always got.
#[derive(Deserialize)]
struct HelpRouteRow {
    run: String,
    #[serde(default)]
    ts: String,
    data: Option<HelpRouteData>,
}

#[derive(Deserialize)]
struct HelpRouteData {
    route: Option<String>,
}

fn fold_newest_help_routes<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> std::collections::HashMap<String, (i64, String)> {
    let mut out = std::collections::HashMap::new();
    for line in lines {
        let Ok(row) = serde_json::from_str::<HelpRouteRow>(line) else {
            continue;
        };
        let ts = chrono::DateTime::parse_from_rfc3339(&row.ts)
            .map(|t| t.timestamp())
            .unwrap_or(0);
        let route = row.data.and_then(|d| d.route).unwrap_or_default();
        match out.get(&row.run) {
            Some((prev, _)) if *prev > ts => {}
            _ => {
                out.insert(row.run, (ts, route));
            }
        }
    }
    out
}

/// The seat's park verdict: the session's newest help route still parks it
/// and the worker has produced nothing since the row landed. Activity in
/// the row's own second is still the help emission itself, which the stop
/// hook routes in that second. A dead worker never parks: no wake lands
/// on a gone session, so recovery stays the ladder's.
fn help_parked(park: Option<&(i64, String)>, activity: i64, dead: bool) -> bool {
    !dead && matches!(park, Some((ts, route)) if *ts >= activity && route_parks_the_seat(route))
}

/// Fold the quota reading into the ladder verdict: a blocked worker's nudge,
/// resume and help all hold until the reset ([`Action::Wait`]), and a
/// transition arms or clears the episode's hold so the caller emits exactly
/// one event per change. `Some(true)` arms, `Some(false)` clears, `None` is
/// quiet. A closed-node stop passes through: freeing a finished seat is not
/// a request against the pool.
fn hold(
    decision: Action,
    armed: &mut Option<i64>,
    reading: Option<i64>,
    now: i64,
) -> (Action, Option<bool>) {
    if !matches!(decision, Action::Nudge(_) | Action::Recover | Action::Help) {
        return (decision, None);
    }
    match reading {
        Some(reset) if reset > now => {
            let changed = *armed != Some(reset);
            *armed = Some(reset);
            (Action::Wait, changed.then_some(true))
        }
        _ => {
            let was = armed.take().is_some();
            (decision, was.then_some(false))
        }
    }
}

#[derive(Debug, PartialEq)]
enum Action {
    Wait,
    Nudge(u8),
    Recover,
    Help,
    /// The node closed under an idle worker: stop it so the seat frees.
    Stop(&'static str),
}

/// The reason a node is closed to its worker, or `None` while work remains.
/// Deferred stays a silent skip: a parked node may come back to the same seat.
pub(super) fn closed(row: &Value) -> Option<&'static str> {
    let field = |k: &str| row.get(k).and_then(Value::as_str);
    match field("status") {
        Some("done") => Some("done"),
        Some("superseded") => Some("superseded"),
        _ if field("superseded_by").is_some_and(|s| !s.is_empty()) => Some("superseded"),
        _ if field("merge_status") == Some("merged") => Some("merged"),
        _ => None,
    }
}

/// On a closed node every move past `Wait` would resume or nudge a worker
/// into work that is already over, so it becomes a stop. A worker still
/// writing its transcript reads `Wait` and is left to finish.
fn on_closed(decision: Action, closed: Option<&'static str>) -> Action {
    match (closed, decision) {
        (_, Action::Wait) => Action::Wait,
        (Some(why), _) => Action::Stop(why),
        (None, decision) => decision,
    }
}

/// The skip a deliberately-stopped row records when its node also delivered:
/// the revival rule's "record a skip reason instead". `None` proceeds into
/// the ladder - an open node's revival stands.
fn stopped_skip_reason(closed: Option<&'static str>) -> Option<String> {
    closed.map(|why| format!("worker stopped on purpose; node {why}; never revived"))
}

/// The newest live ruling's `(decision_id, decision text)` on `node` when it
/// parks the seat: max by the `(ts, decision_id)` pair among rows whose
/// subject names the node (ties take the later row, as `max_by_key` did),
/// taken only when its text opens with "park".
/// `None` means the seat is not parked. The id rides the sort key it was
/// already read into, so surfacing it costs no extra field read. The resume
/// admission door quotes both in its refusal; the quiet tick reads it
/// through the bool wrapper below, so the pick, the park test and the two
/// readers share one implementation.
pub(crate) fn parked_by_newest_ruling_text<'a>(
    rows: &'a [Value],
    node: &str,
) -> Option<(String, &'a str)> {
    let mut best: Option<(&Value, (String, String))> = None;
    for row in rows
        .iter()
        .filter(|r| r.get("subject").and_then(Value::as_str) == Some(node))
    {
        let key = (
            row.get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            row.get("decision_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        if best.as_ref().map_or(true, |(_, k)| key >= *k) {
            best = Some((row, key));
        }
    }
    let (row, key) = best?;
    let text = row
        .get("decision")
        .and_then(Value::as_str)
        .filter(|text| text.trim().to_lowercase().starts_with("park"))?;
    Some((key.1, text))
}

/// The newest live ruling on `node` parks the seat when its text opens with
/// "park": the coord convention a lead records to hold a node idle ("Park
/// the node under ..."). The newest ruling governs, so a later unpark or any
/// newer decision lifts the park by being newest; retraction, supersession
/// and date expiry lift it by dropping the row from the live set. An
/// unreadable store parks nothing: a damaged decision index must never
/// strand quiet workers. The parked resume route's revive arm reads the same
/// predicate, so the watchdog and the resume door cannot disagree on what
/// "parked" means.
pub(crate) fn parked_by_newest_ruling(rows: &[Value], node: &str) -> bool {
    parked_by_newest_ruling_text(rows, node).is_some()
}

/// The route kinds whose wake owner is not the quiet ladder: a `timer`
/// wait is the help-router sweep's to fire, and an off-session delivery,
/// its ladder climbs, and a user-page filing all wait on a person. A
/// `ruled` row names a settled escalation and an `in-session` row names a
/// session the stop hook is still driving. An unknown kind holds nothing:
/// a missed park re-wakes a parked worker, while a wrong park strands a
/// worker whose only waker now refuses.
fn route_parks_the_seat(route: &str) -> bool {
    matches!(route, "timer" | "off-session" | "climbed" | "question-page")
}

fn action(episode: &mut Episode, tail: &Tail, now: i64, live: bool, dead: bool) -> Action {
    if episode.activity == 0 {
        episode.activity = tail.started_at.unwrap_or_default();
    }
    if tail.activity > episode.activity {
        *episode = Episode {
            activity: tail.activity,
            ..Default::default()
        };
    }
    if episode.escalated {
        return Action::Wait;
    }
    // A pid probe can read a working worker as gone (a launcher pid, a tool
    // call in flight). A live own claim or a transcript written inside the
    // first nudge window outranks it: no resume, no salvage, no help.
    let fresh = now.saturating_sub(tail.activity.max(tail.touched)) < 900;
    if dead && !live && !fresh {
        return if episode.recovered {
            Action::Help
        } else {
            Action::Recover
        };
    }
    if let Some((_, _, stage)) = episode.pending.as_ref() {
        if tail.read {
            episode.read_stage = *stage;
            episode.pending = None;
        } else if fresh {
            return Action::Wait;
        } else {
            return if episode.recovered {
                Action::Help
            } else {
                Action::Recover
            };
        }
    }
    if !live {
        return Action::Wait;
    }
    if episode.activity == 0 && !tail.failed {
        return Action::Wait;
    }
    if tail.failed && episode.read_stage == 0 {
        return Action::Nudge(1);
    }
    let quiet = now.saturating_sub(episode.activity);
    if quiet >= 2700 && episode.read_stage >= 2 {
        Action::Help
    } else if quiet >= 1800 && episode.read_stage == 1 {
        Action::Nudge(2)
    } else if quiet >= 900 && episode.read_stage == 0 {
        Action::Nudge(1)
    } else {
        Action::Wait
    }
}

fn command(args: &[&str], cwd: &str) -> (i32, String, String) {
    crate::burn_watch::run_command(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), cwd)
}

/// `{out} {error}` condensed to its last non-empty line - stderr's when
/// stderr carries one, else stdout's - so a Python traceback's dozens of
/// `File "..."` frames never reach a help reason, only its final line.
fn condensed(out: &str, error: &str) -> String {
    let from_error = crate::attention_arm::last_line(error);
    if !from_error.is_empty() {
        from_error
    } else {
        crate::attention_arm::last_line(out)
    }
}

/// The thread resume argv `restore` runs with no session identity of its
/// own: named `fno/quiet-recovery` so the mail envelope's sender guard sees
/// a real arm, not a bare `fno`.
fn resume_argv<'a>(sid: &'a str, text: &'a str) -> [&'a str; 9] {
    [
        "fno",
        "agents",
        "session",
        "resume",
        sid,
        "--message",
        text,
        "--from-name",
        SENDER,
    ]
}

fn help(entry: &RegistryEntry, scope: &str, node: &str, reason: &str) -> Result<(), String> {
    let text = format!("control: <help class=\"stuck\" reason=\"unfinished-worker\">{node}: {} in {}: {reason}. Review the unfinished work and take ownership of this node.</help>", entry.name, entry.cwd);
    let (code, out, error) = command(
        &[
            "fno",
            "agents",
            "mail",
            "send",
            "--from-name",
            SENDER,
            "--origin",
            "scheduler",
            "--to-lead",
            scope,
            &text,
        ],
        &entry.cwd,
    );
    if crate::mail_inject::mail_send_accepted(code, &out) {
        Ok(())
    } else {
        Err(format!("lead delivery refused: {out} {error}"))
    }
}

fn send_recovery(entry: &RegistryEntry, sid: &str, text: &str) -> Result<(), String> {
    let (code, out, error) = command(
        &[
            "fno",
            "agents",
            "mail",
            "send",
            "--from-name",
            SENDER,
            "--origin",
            "scheduler",
            sid,
            text,
        ],
        &entry.cwd,
    );
    if crate::mail_inject::mail_send_accepted(code, &out)
        || crate::mail_inject::mail_send_receipt(&out).contains("durable")
    {
        Ok(())
    } else {
        Err(format!(
            "recovery message refused: {}",
            condensed(&out, &error)
        ))
    }
}

fn restore(entry: &RegistryEntry, sid: &str, text: &str) -> Result<(), String> {
    // Recovery consumes only the resume recipe, not a persisted native locator.
    let receipt = crate::receipt::build_reap_receipt(
        entry,
        None,
        crate::receipt::Writer::RegistryWrite,
        &|_| None,
    )?;
    if !Path::new(&receipt.cwd).is_dir() {
        return Err(format!(
            "recorded worktree is gone; resume: {}",
            receipt.resume
        ));
    }
    if let Some(mux) = entry.mux.as_ref() {
        let (code, out, error) = command(
            &[
                "fno",
                "mux",
                "workspace",
                "restore",
                "--session",
                &mux.session,
                "--member-session",
                sid,
                "--json",
            ],
            &receipt.cwd,
        );
        let answer: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
        if code == 0
            && answer
                .get("members")
                .and_then(Value::as_array)
                .is_some_and(|members| {
                    members.iter().any(|row| {
                        matches!(
                            row.get("outcome").and_then(Value::as_str),
                            Some("resumed" | "focused")
                        )
                    })
                })
        {
            return send_recovery(entry, sid, text);
        }
        return Err(format!(
            "workspace restore refused: {}; resume: {}",
            condensed(&out, &error),
            receipt.resume
        ));
    }
    let (code, out, error) = command(&resume_argv(sid, text), &receipt.cwd);
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "thread resume refused: {}; resume: {}",
            condensed(&out, &error),
            receipt.resume
        ))
    }
}

fn salvage(
    entry: &RegistryEntry,
    sid: &str,
    now: i64,
    claude_home: &crate::claude_ask::ClaudeHome,
) -> Result<String, String> {
    // The registry's recorded cwd can still be canonical (a worker launched
    // there, then ran EnterWorktree); the transcript names where it actually
    // ended up. Salvaging the recorded cwd in that case commits canonical's
    // own stray files under a `recovery/` ref - never the worker's.
    let tree = crate::reentry::resolve_resume_cwd(claude_home, entry.cwd.as_str(), sid);
    if crate::canonical_check::is_canonical_checkout(&tree) {
        return Err(format!(
            "salvage skipped: {} is the main worktree, not the worker's",
            tree.display()
        ));
    }
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index_path = temp.path().join("index");
    let run = |args: &[&str]| -> Result<String, String> {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&tree)
            .env("GIT_INDEX_FILE", &index_path)
            .args(args);
        let out =
            crate::bounded_cmd::output_with_timeout_result(cmd, 30).map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).to_string())
        }
    };
    let head = run(&["rev-parse", "HEAD"])?.trim().to_string();
    run(&["read-tree", "HEAD"])?;
    run(&["add", "--update"])?;
    let untracked = run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut add = vec!["add", "--"];
    add.extend(untracked.split('\0').filter(|name| !name.is_empty()));
    if add.len() > 2 {
        run(&add)?;
    }
    let tree = run(&["write-tree"])?.trim().to_string();
    let commit = run(&[
        "commit-tree",
        &tree,
        "-p",
        &head,
        "-m",
        "chore: preserve unfinished worker changes",
    ])?
    .trim()
    .to_string();
    let branch = format!("refs/heads/recovery/{sid}-{now}");
    run(&[
        "update-ref",
        &branch,
        &commit,
        "0000000000000000000000000000000000000000",
    ])?;
    Ok(format!(
        "salvaged {branch} at {commit}; original index and worktree preserved"
    ))
}

/// The claude listing reader behind [`vendor_stopped`]. In tests,
/// `VENDOR_LISTING` overrides the real read (a unit test cannot populate
/// the live roster, and reading it would spawn the claude CLI), the same
/// posture as `restore_gate`'s registry override.
fn vendor_listing() -> crate::claude_roster::ClaudeAgentsSnapshot {
    #[cfg(test)]
    if let Some(snapshot) = VENDOR_LISTING.with(|slot| slot.borrow().clone()) {
        return snapshot;
    }
    crate::claude_roster::read_all_agents_union()
}

#[cfg(test)]
thread_local! {
    static VENDOR_LISTING: std::cell::RefCell<Option<crate::claude_roster::ClaudeAgentsSnapshot>> =
        const { std::cell::RefCell::new(None) };
}

/// The vendor's own stop word for a claude row, read once per pass on
/// first need. A `claude stop` run outside fno journals no `agent_stopped`
/// row, so the listing's `stopped` word is the only stop record a
/// vendor-side stop ever produces (2026-11-11 specimen: two workers their
/// lead stopped at the claude CLI were revived because no journal row
/// existed). Only the deliberate word holds the seat: `done` and `failed`
/// are the words a worker reaches on its own, and their revival ladder
/// stays. An unreadable or missing listing is unknown, never stopped -
/// a broken instrument must not strand a revivable worker.
fn vendor_stopped(
    entry: &RegistryEntry,
    listing: &mut Option<crate::claude_roster::ClaudeAgentsSnapshot>,
) -> bool {
    if entry.harness_kind() != Harness::Claude {
        return false;
    }
    let snapshot = listing.get_or_insert_with(vendor_listing);
    crate::daemon::roster_death::claude_row_id(entry)
        .and_then(|row_id| snapshot.find(&row_id))
        .and_then(|row| row.state.as_deref())
        .is_some_and(|word| word.trim() == "stopped")
}

pub(super) fn run(home: &AgentsHome) -> Result<(), String> {
    // A fleet stop that holds spawns orders every worker idle: a resume is
    // re-admission, the scope the loop pause reads for the same reason.
    // Hold every nudge, resume, help and stop until the all-clear; an
    // unreadable breaker holds the way every admission door reads it.
    if crate::fleet_incident::read_at(&crate::fleet_incident::fleet_stop_path(home)).holds("spawns")
    {
        return Ok(());
    }
    let registry = crate::state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?;
    let rows = crate::graph_store::read_rows_where(
        &crate::gc_sweep::graph_path(home),
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "status",
                    "merge_status",
                    "superseded_by",
                    "sessions",
                    "parent",
                    "project",
                    "type",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    // The live rulings the decisions surface renders, loaded once per tick:
    // the park gate reads the same set `fno backlog decisions` shows.
    let rulings = crate::decision_index::default_store_live()
        .map(|index| index.rows)
        .unwrap_or_default();
    // The newest help route per session, folded once per tick: the park
    // shield reads the same rows the help-router sweep acts on.
    let help_parks = newest_help_routes(&home.events_jsonl());
    let now = chrono::Utc::now().timestamp();
    let claude_home = crate::claude_ask::ClaudeHome::from_env();
    let watches = crate::watch_expiry::read_evidence(home, now * 1000)?;
    // The evidence reader stamps each `agent_stopped` row with the session
    // that held the stopped name at the time, so a respawn under the same
    // name keeps its own recovery.
    let stopped: std::collections::HashSet<_> = watches
        .iter()
        .filter(|e| e.kind == "agent_stopped")
        .filter_map(|e| e.session_id.as_deref())
        .collect();
    let mut stores = crate::gc_inventory::HarnessStoreIndex::default();
    let mut loaded = None;
    let mut codex_daemon_dead = None;
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("quiet-recovery");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let settle_watches = super::settle_watches(&watches);
    let mut owners = None;
    let mut vendor_listing_memo: Option<crate::claude_roster::ClaudeAgentsSnapshot> = None;
    let mut acted = 0;
    for entry in &registry.entries {
        let result = (|| -> Result<(), String> {
            let Some(sid) = entry
                .harness_session_id
                .as_deref()
                .filter(|s| crate::resume_wake::is_uuid_shaped(s))
            else {
                return Ok(());
            };
            let Some(node) = entry.node.as_deref() else {
                return Ok(());
            };
            let Some(row) = rows
                .iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(node))
            else {
                return Ok(());
            };
            if row.get("status").and_then(Value::as_str) == Some("deferred") {
                return Ok(());
            }
            let closed = closed(row);
            // A node whose newest ruling parks it holds any worker in the
            // seat: the ruling, not quiet recovery, decides when the work
            // resumes. A closed node keeps its stop-and-free-seat ritual.
            if closed.is_none() && parked_by_newest_ruling(&rulings, node) {
                return Ok(());
            }
            if settle_watches.iter().any(|w| {
                (w.watch.session_id == sid || w.codex_thread_id.as_deref() == Some(sid))
                    && watch_shielded(&w.watch, &watches, now * 1000)
            }) {
                return Ok(());
            }
            let (claim_state, claim) = crate::claims::status(&format!("node:{node}"), None);
            let own = claim.as_ref().is_some_and(|c| {
                c.session_id.as_deref() == Some(sid) || c.holder == format!("target-session:{sid}")
            });
            if matches!(
                claim_state,
                crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
            ) && !own
            {
                return Ok(());
            }
            if claim_state == crate::claims::ClaimState::Corrupted {
                return Ok(());
            }
            // A row a lead stopped on purpose is the lead's, not quiet
            // recovery's: send no nudge, no resume and no help for it. When
            // its work also delivered, the revival rule records the skip it
            // mandates; an open node stays the silent grace skip. A claude
            // stop run outside fno journals nothing, so its stop is read
            // off the vendor listing further down, once the row is dead.
            if stopped.contains(sid) {
                if let Some(reason) = stopped_skip_reason(closed) {
                    emitter
                        .emit(
                            "quiet_worker_skip",
                            &json!({"session_id": sid, "node": node, "reason": reason}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
            let state_path = dir.join(format!("{sid}.json"));
            let mut episode: Episode = match std::fs::read(&state_path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Episode::default(),
                Err(e) => return Err(e.to_string()),
            };
            let path = entry
                .transcript_path
                .as_deref()
                .filter(|p| Path::new(p).is_file())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    stores
                        .matches(entry)?
                        .into_iter()
                        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
                });
            let native_codex_thread = entry.harness_kind() == Harness::Codex && entry.mux.is_none();
            let daemon_dead = native_codex_thread
                && *codex_daemon_dead.get_or_insert_with(|| {
                    let adapter = crate::codex_inject::CodexDaemonAdapter::from_environment();
                    adapter.provider_pid_start().map_or_else(
                        || !adapter.socket_up(),
                        |(pid, start)| !crate::daemon::pid_is_ours(pid, Some(start)),
                    )
                });
            let thread_unloaded = native_codex_thread
                && !daemon_dead
                && loaded
                    .get_or_insert_with(crate::codex_inject::loaded_thread_ids)
                    .as_ref()
                    .is_ok_and(|roster| !roster.contains(sid));
            let dead = matches!(
                entry.status,
                crate::AgentStatus::Failed
                    | crate::AgentStatus::Exited
                    | crate::AgentStatus::PermanentDead
            ) || entry.pid.is_some_and(|pid| crate::daemon::pid_is_gone(pid))
                || daemon_dead
                || thread_unloaded;
            // A claude row fno reads dead whose vendor word reads stopped
            // is a deliberate stop the journal never recorded. The listing
            // read is spent only on dead rows, never on a healthy fleet's
            // pass: a vendor-stopped row's pid is gone, so it reads dead
            // on the same probe the rest of the ladder uses.
            if dead && vendor_stopped(entry, &mut vendor_listing_memo) {
                return Ok(());
            }
            let transcript_tail = match path.as_deref() {
                Some(path) => tail(path, episode.pending.as_ref().map(|p| p.0.as_str())),
                None if dead => Some(Tail::default()),
                None => None,
            };
            let Some(tail) = transcript_tail else {
                return Ok(());
            };
            // A tool call still open inside its own timeout is a worker in
            // a foreground wait (a REST loop, a long test run). Not quiet.
            // A dead pid cannot be waiting on its own call, so it skips the hold.
            if !dead && tail.call_open(now) {
                return Ok(());
            }
            // A worker whose newest help route still parks it waits on the
            // route's own owner, the sweep's timer or a human answer, so the
            // ladder holds. Activity newer than the row lifts the park back
            // into the ordinary ladder: the worker is back, or the owner's
            // wake just landed.
            if help_parked(help_parks.get(sid), tail.activity, dead) {
                return Ok(());
            }
            let read_receipt = episode
                .pending
                .as_ref()
                .filter(|_| tail.read)
                .map(|(marker, _, stage)| (marker.clone(), *stage));
            let decision = on_closed(
                action(
                    &mut episode,
                    &tail,
                    now,
                    own && claim_state == crate::claims::ClaimState::Live,
                    dead,
                ),
                closed,
            );
            // A worker blocked on a provider quota wall reads exactly like a
            // stuck one: silent transcript, failing tail. Every nudge, resume
            // and help mail is one more rejected request against the shared
            // pool, so the ladder holds until the 429's reset; the check-in's
            // lane reading carries the block instead.
            let quota_reading =
                if matches!(decision, Action::Nudge(_) | Action::Recover | Action::Help) {
                    quota_reset_ahead(path.as_deref(), entry.harness_kind() == Harness::Codex, now)
                } else {
                    None
                };
            let (decision, quota_event) =
                hold(decision, &mut episode.quota_reset_at, quota_reading, now);
            if let Some(held) = quota_event {
                let row = match quota_reading {
                    Some(reset) => json!({"session_id": sid, "node": node, "held": held,
                        "reset_at_iso": crate::provider_cap::epoch_to_rfc3339(reset)}),
                    None => json!({"session_id": sid, "node": node, "held": held}),
                };
                emitter
                    .emit("quiet_worker_quota_hold", &row)
                    .map_err(|e| e.to_string())?;
            }
            if decision != Action::Wait {
                acted += 1;
            }
            let mut handoff = None;
            match decision {
                Action::Wait => {}
                Action::Nudge(stage) => {
                    let marker = format!("quiet-recovery:{sid}:{}:{stage}", episode.activity);
                    let receipt = crate::receipt::build_reap_receipt(
                        entry,
                        None,
                        crate::receipt::Writer::RegistryWrite,
                        &|_| None,
                    )?;
                    let text = if stage == 1 {
                        if tail.failed {
                            format!("control: previous turn failed on {node}. Continue the assigned work or emit <help>. [{marker}]")
                        } else {
                            format!("control: quiet 15 min on {node}, what blocks you? continue or emit <help>. [{marker}]")
                        }
                    } else {
                        format!("control: quiet 30 min on {node}. Continue or emit <help>. Exact harness resume: {} [{marker}]", receipt.resume)
                    };
                    // A transport ack is only handed. The next transcript read
                    // confirms read before this attempt can advance the ladder.
                    episode.pending = Some((marker.clone(), now, stage));
                    let (code, out, error) = command(
                        &[
                            "fno",
                            "agents",
                            "mail",
                            "send",
                            "--from-name",
                            SENDER,
                            "--origin",
                            "scheduler",
                            sid,
                            &text,
                        ],
                        &entry.cwd,
                    );
                    let delivery = if crate::mail_inject::mail_send_accepted(code, &out) {
                        "handed"
                    } else if crate::mail_inject::mail_send_receipt(&out).contains("durable") {
                        "queued"
                    } else {
                        "failed"
                    };
                    emitter
                        .emit(
                            "quiet_worker_nudge",
                            &json!({"session_id": sid, "node": node, "stage": stage,
                    "marker": marker, "delivery": delivery, "exit": code, "receipt": out, "error": error}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                Action::Recover => {
                    let marker = format!("quiet-recovery:{sid}:{}:resume", episode.activity);
                    let text = format!("control: The previous worker stopped with unfinished work. Continue the assigned work in this same worktree from the last completed step, preserving all changes. [{marker}]");
                    let result = restore(entry, sid, &text);
                    episode.recovered = true;
                    let reason = match &result {
                        Ok(()) => format!("resume sent; transcript read pending [{marker}]"),
                        Err(error) => format!(
                            "{error}; {}",
                            salvage(entry, sid, now, &claude_home).unwrap_or_else(|e| format!(
                                "salvage refused: {e}; preserve worktree {}",
                                entry.cwd
                            ))
                        ),
                    };
                    emitter
                        .emit(
                            "quiet_worker_recovery",
                            &json!({"session_id": sid, "node": node, "reason": reason}),
                        )
                        .map_err(|e| e.to_string())?;
                    if result.is_err() {
                        episode.recovery_reason = Some(reason.clone());
                        handoff = Some(reason);
                    } else {
                        episode.recovery_reason = Some(reason.clone());
                        episode.pending = Some((marker, now, 3));
                    }
                }
                Action::Stop(why) => {
                    let (code, out, error) = command(&["fno", "agents", "stop", sid], &entry.cwd);
                    episode.escalated = true;
                    let receipt =
                        crate::attention_arm::last_line(if code == 0 { &out } else { &error });
                    emitter
                        .emit(
                            "quiet_worker_recovery",
                            &json!({"session_id": sid, "node": node,
                        "reason": format!("node {why}: stopped the idle worker instead of resuming it (exit {code}: {receipt})")}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                Action::Help => {
                    handoff = Some(episode.recovery_reason.clone().unwrap_or_else(|| {
                        "worker stayed quiet after read-confirmed nudges".to_string()
                    }));
                }
            }
            if let Some((marker, stage)) = read_receipt {
                if stage == 3 {
                    emitter
                        .emit(
                            "quiet_worker_recovery",
                            &json!({"session_id": sid, "node": node,
                        "reason": format!("recipient transcript confirms recovery read [{marker}]"),
                        "delivery": "read"}),
                        )
                        .map_err(|e| e.to_string())?;
                } else {
                    emitter
                        .emit(
                            "quiet_worker_nudge",
                            &json!({"session_id": sid, "node": node,
                        "stage": stage, "marker": marker, "delivery": "read"}),
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
            let bytes = serde_json::to_vec(&episode).map_err(|e| e.to_string())?;
            let tmp = state_path.with_extension("tmp");
            std::fs::write(&tmp, bytes)
                .and_then(|_| std::fs::rename(tmp, &state_path))
                .map_err(|e| e.to_string())?;
            if let Some(reason) = handoff {
                let scope = crate::burn_watch::node_owner(
                    Path::new(&entry.cwd),
                    &home.registry_json(),
                    &rows,
                    &mut owners,
                    node,
                )
                .ok_or_else(|| format!("no live team owner resolved for node {node}"))?;
                help(entry, &scope, node, &reason)?;
                episode.escalated = true;
                let bytes = serde_json::to_vec(&episode).map_err(|e| e.to_string())?;
                let tmp = dir.join(format!("{sid}.tmp"));
                std::fs::write(&tmp, bytes)
                    .and_then(|_| std::fs::rename(tmp, &state_path))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            emitter
                .emit(
                    "quiet_worker_error",
                    &json!({"name": entry.name, "reason": error}),
                )
                .map_err(|e| e.to_string())?;
        }
        if acted >= 4 {
            break;
        }
    }
    prune_salvage_refs(&registry, &dir, now)?;
    Ok(())
}

/// Salvage refs are checked against git at most once an hour: every check
/// reads each stray path, too much to pay on every daemon tick.
const SALVAGE_PRUNE_INTERVAL_SECS: i64 = 3600;

fn prune_salvage_refs(
    registry: &crate::state::Registry,
    dir: &Path,
    now: i64,
) -> Result<(), String> {
    let stamp = dir.join("salvage-prune.epoch");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    if matches!(last, Some(t) if now - t < SALVAGE_PRUNE_INTERVAL_SECS) {
        return Ok(());
    }
    let repos: std::collections::BTreeSet<&str> =
        registry.entries.iter().map(|e| e.cwd.as_str()).collect();
    for repo in repos {
        let pruned = match super::salvage_prune::prune(Path::new(repo), "origin/main", now) {
            Ok(pruned) => pruned,
            Err(error) => {
                eprintln!("salvage prune skipped {repo}: {error}");
                continue;
            }
        };
        for p in pruned {
            eprintln!("salvage ref pruned {}: {}", p.refname, p.sha);
        }
    }
    std::fs::write(&stamp, now.to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One assistant row: real work, or a quota 429 naming its reset.
    fn quota_row(ts: i64, reset: Option<i64>) -> String {
        let stamp = reset
            .map(|r| format!(" reset at {}", crate::provider_cap::epoch_to_rfc3339(r)))
            .unwrap_or_default();
        format!(
            concat!(
                r#"{{"timestamp":"{ts}","type":"assistant","isApiErrorMessage":true,"#,
                r#""message":{{"content":[{{"type":"text","text":"API Error: 429 usage limit{stamp}"}}]}}}}"#,
                "\n"
            ),
            ts = crate::provider_cap::epoch_to_rfc3339(ts),
            stamp = stamp,
        )
    }

    fn transcript_with(rows: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, rows.join("\n")).unwrap();
        (dir, path)
    }

    #[test]
    fn an_open_tool_call_reads_not_quiet_until_its_own_timeout_then_quiet() {
        // The Bash call opens at T with a 10-minute timeout and no result
        // row: the worker is inside a foreground wait. Past the timeout the
        // call is stale and the ladder reads the worker quiet again.
        let opened = chrono::DateTime::parse_from_rfc3339("2026-11-25T10:00:00Z")
            .unwrap()
            .timestamp();
        let (_dir, path) = transcript_with(&[
            r#"{"type":"assistant","timestamp":"2026-11-25T10:00:00Z","message":{"content":[{"type":"tool_use","id":"toolu_open","name":"Bash","input":{"command":"rest-loop","timeout":600000}}]}}"#,
        ]);
        let tail = tail(&path, None).unwrap();
        assert!(
            tail.call_open(opened + 60),
            "an open call inside its timeout is not quiet"
        );
        assert!(
            tail.call_open(opened + 599),
            "the call still holds one second before its bound"
        );
        assert!(
            !tail.call_open(opened + 601),
            "the same call past its timeout reads quiet"
        );
    }

    #[test]
    fn a_closed_tool_call_never_holds_the_ladder() {
        let (_dir, path) = transcript_with(&[
            r#"{"type":"assistant","timestamp":"2026-11-25T10:00:00Z","message":{"content":[{"type":"tool_use","id":"toolu_done","name":"Bash","input":{"command":"ls","timeout":600000}}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-25T10:00:05Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_done","content":"ok"}]}}"#,
        ]);
        let tail = tail(&path, None).unwrap();
        let opened = chrono::DateTime::parse_from_rfc3339("2026-11-25T10:00:00Z")
            .unwrap()
            .timestamp();
        assert!(
            !tail.call_open(opened + 60),
            "a call with a result is not open"
        );
    }

    #[test]
    fn a_codex_function_call_without_output_reads_not_quiet_inside_its_timeout() {
        let (_dir, path) = transcript_with(&[
            r#"{"type":"response_item","timestamp":"2026-11-25T10:00:00Z","payload":{"type":"function_call","name":"shell","call_id":"call_open","arguments":"{\"command\":[\"sleep\",\"900\"],\"timeout_ms\":1200000}"}}"#,
        ]);
        let tail = tail(&path, None).unwrap();
        let opened = chrono::DateTime::parse_from_rfc3339("2026-11-25T10:00:00Z")
            .unwrap()
            .timestamp();
        assert!(
            tail.call_open(opened + 900),
            "a codex call inside its timeout is not quiet"
        );
        assert!(
            !tail.call_open(opened + 1201),
            "past its timeout it reads quiet"
        );
    }

    #[test]
    fn quota_tail_holds_only_for_a_future_reset() {
        // The verdict scan and the stamp parse run over the in-memory lines:
        // the same contracts `quota_reset_ahead` composes, without the file
        // round-trip whose read this runner's tests cannot order reliably.
        let now = chrono::Utc::now().timestamp();
        let row = quota_row(now - 2000, Some(now + 3600));
        let lines: Vec<String> = row.lines().map(String::from).collect();
        let (ts, excerpt, unknown) =
            crate::provider_cap::capped_tail_lines(&lines, row.len() as u64);
        assert!(
            excerpt.is_some(),
            "the scan said {unknown:?} (ts {ts:?}); lines: {n}; marker: {marker}; serde: {serde_ok:?}; row: {row:?}",
            n = lines.len(),
            marker = lines.iter().any(|l| l.contains("\"type\":\"assistant\"")),
            serde_ok = lines
                .first()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()),
        );
        let reset = crate::provider_cap::reset_epoch_from_excerpt(
            excerpt.as_deref().expect("quota excerpt"),
            None,
            now,
        );
        assert_eq!(reset, Some(now + 3600), "the offset stamp parses");
    }

    #[test]
    fn quota_tail_reads_not_blocked_for_passed_reset_or_clean_work() {
        let now = chrono::Utc::now().timestamp();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(&path, quota_row(now - 2000, Some(now - 60))).unwrap();
        assert_eq!(quota_reset_ahead(Some(&path), false, now), None);
        // A 429 with no parsable reset holds one pool window from the error.
        std::fs::write(&path, quota_row(now - 30, None)).unwrap();
        let reset = quota_reset_ahead(Some(&path), false, now).unwrap_or_else(|| {
            panic!(
                "a resetless 429 still holds one window; the tail reading: {:?}",
                crate::provider_cap::capped_tail(&path)
            )
        });
        assert!(
            reset >= now + QUOTA_UNKNOWN_RESET_HOLD_S - 60,
            "the fallback holds one 5h window from the error stamp: {reset}"
        );
        // Real work at the tail is never a hold.
        let work = format!(
            r#"{{"timestamp":"{ts}","type":"assistant","message":{{"content":[{{"type":"text","text":"building the gate"}}]}}}}"#,
            ts = crate::provider_cap::epoch_to_rfc3339(now - 30),
        );
        std::fs::write(&path, work).unwrap();
        assert_eq!(quota_reset_ahead(Some(&path), false, now), None);
    }

    #[test]
    fn the_hold_folds_the_ladder_and_reports_each_transition() {
        let now = 1_000_000i64;
        let mut armed = None;
        let reading = |reset: i64| Some(reset);
        let (decision, event) = hold(Action::Nudge(1), &mut armed, reading(now + 3600), now);
        assert_eq!(
            decision,
            Action::Wait,
            "the nudge holds under a future reset"
        );
        assert_eq!(event, Some(true), "arming emits once");
        let (decision, event) = hold(Action::Nudge(1), &mut armed, reading(now + 3600), now);
        assert_eq!(decision, Action::Wait);
        assert_eq!(event, None, "an unchanged hold is silent");
        let (decision, event) = hold(Action::Nudge(1), &mut armed, None, now);
        assert_eq!(decision, Action::Nudge(1), "the nudge fires once unblocked");
        assert_eq!(event, Some(false), "clearing emits once");
        assert_eq!(armed, None);
        let (decision, _) = hold(Action::Stop("done"), &mut armed, reading(now + 3600), now);
        assert_eq!(
            decision,
            Action::Stop("done"),
            "a closed-node stop passes through"
        );
    }

    #[test]
    fn ladder_requires_reads_resets_on_items_and_recovers_unfinished_death() {
        let mut e = Episode {
            activity: 100,
            ..Default::default()
        };
        let mut t = Tail {
            activity: 100,
            ..Default::default()
        };
        assert_eq!(action(&mut e, &t, 1000, true, false), Action::Nudge(1));
        e.pending = Some(("marker".into(), 1000, 1));
        t.touched = 950;
        assert_eq!(
            action(&mut e, &t, 1001, true, false),
            Action::Wait,
            "an unread nudge never recovers a worker still writing its transcript"
        );
        t.touched = 0;
        assert_eq!(action(&mut e, &t, 1001, true, false), Action::Recover);
        assert_eq!(action(&mut e, &t, 1600, true, false), Action::Recover);
        t.read = true;
        assert_eq!(action(&mut e, &t, 1900, true, false), Action::Nudge(2));
        e.pending = Some(("second".into(), 1900, 2));
        assert_eq!(action(&mut e, &t, 2800, true, false), Action::Help);
        e.pending = Some(("unread".into(), 2700, 2));
        t.read = false;
        assert_eq!(action(&mut e, &t, 2801, false, true), Action::Recover);
        e.pending = None;
        t.activity = 2800;
        assert_eq!(action(&mut e, &t, 2801, true, false), Action::Wait);
        assert_eq!(e.read_stage, 0);
        assert_eq!(
            action(&mut e, &t, 2801, false, true),
            Action::Wait,
            "fresh transcript work outranks a gone pid"
        );
        assert_eq!(
            action(&mut e, &t, 4000, true, true),
            Action::Nudge(1),
            "a live own claim outranks a gone pid: the quiet ladder runs, not recovery"
        );
        assert_eq!(action(&mut e, &t, 4000, false, true), Action::Recover);
        t.touched = 3900;
        e.recovered = false;
        assert_eq!(
            action(&mut e, &t, 4000, false, true),
            Action::Wait,
            "a transcript file write mid tool call is progress"
        );
        assert_eq!(action(&mut e, &t, 5000, false, false), Action::Wait);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, concat!(
            "{\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\"}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"text\":\"marker\"}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:02Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"error\":{\"message\":\"routing timeout\"}}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"text\":\"retry started\"}}\n"
        )).unwrap();
        let evidence = tail(&path, Some("marker")).unwrap();
        assert_eq!(evidence.activity, 1767226503);
        assert!(
            !evidence.failed,
            "new transcript work clears stale failure state"
        );
        assert!(
            evidence.read,
            "receipt bytes in the recipient transcript confirm the nudge"
        );

        std::fs::write(
            &path,
            "{\"timestamp\":\"2026-01-01T00:15:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        assert_eq!(tail(&path, None).unwrap().activity, 1767226500);
    }

    #[test]
    fn closed_node_stops_an_idle_worker_instead_of_resuming_it() {
        let superseded = json!({"id": "x-1", "status": "in_progress", "superseded_by": "x-2"});
        assert_eq!(closed(&superseded), Some("superseded"));
        assert_eq!(
            on_closed(Action::Recover, closed(&superseded)),
            Action::Stop("superseded")
        );
        assert_eq!(
            on_closed(Action::Nudge(1), closed(&json!({"status": "done"}))),
            Action::Stop("done")
        );
        assert_eq!(
            on_closed(Action::Wait, Some("merged")),
            Action::Wait,
            "a worker still writing on a merged node finishes its ritual"
        );
        let open = json!({"status": "in_progress", "superseded_by": null});
        assert_eq!(on_closed(Action::Recover, closed(&open)), Action::Recover);
    }

    #[test]
    fn a_stopped_row_records_its_skip_only_on_a_delivered_node() {
        for why in ["done", "superseded", "merged"] {
            let reason = stopped_skip_reason(Some(why)).expect("delivered nodes record a skip");
            assert!(
                reason.contains(why) && reason.contains("never revived"),
                "{reason}"
            );
        }
        assert!(
            stopped_skip_reason(None).is_none(),
            "an open node's stopped worker keeps the silent grace skip"
        );
    }

    #[test]
    fn the_vendor_stopped_word_stands_in_for_a_missing_stop_record() {
        let claude_row = |short_id: Option<&str>, sid: Option<&str>| {
            let mut entry = RegistryEntry::default();
            entry.name = "t-vendor-stop-opus".into();
            entry.harness = Some("claude".into());
            entry.short_id = short_id.unwrap_or_default().into();
            entry.harness_session_id = sid.map(str::to_string);
            entry
        };
        let listing_with = |state: Option<&str>| {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("2645b710", state),
            ])
        };
        // The 2026-11-11 specimen ids: the lead stopped the workers at the
        // claude CLI, so no agent_stopped row exists; the listing carries
        // the stop instead.
        let mut stopped = Some(listing_with(Some("stopped")));
        assert!(vendor_stopped(
            &claude_row(Some("2645b710"), None),
            &mut stopped
        ));
        let mut prefixed = Some(listing_with(Some("stopped")));
        assert!(
            vendor_stopped(
                &claude_row(None, Some("2645b710-862f-463a-9959-fe3c21690ffc")),
                &mut prefixed
            ),
            "the session-id prefix answers when the row carries no short id"
        );
        // Every other word is a word the worker reached on its own, and an
        // unreadable or absent listing is unknown: none of them holds the
        // seat.
        for state in ["done", "failed", "working"] {
            let mut listing = Some(listing_with(Some(state)));
            assert!(
                !vendor_stopped(&claude_row(Some("2645b710"), None), &mut listing),
                "{state}"
            );
        }
        let mut absent = Some(listing_with(None));
        assert!(!vendor_stopped(
            &claude_row(Some("2645b710"), None),
            &mut absent
        ));
        let mut unreadable = Some(crate::claude_roster::ClaudeAgentsSnapshot::unknown("boom"));
        assert!(!vendor_stopped(
            &claude_row(Some("2645b710"), None),
            &mut unreadable
        ));
        // A codex row has no claude listing to consult.
        let mut codex = RegistryEntry::default();
        codex.name = "t-x-glm".into();
        codex.harness = Some("codex".into());
        codex.short_id = "2645b710".into();
        let mut fresh = None;
        assert!(!vendor_stopped(&codex, &mut fresh));
    }

    #[test]
    fn a_worker_a_lead_stopped_stays_stopped_when_quiet_recovery_runs() {
        let base = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(base.path().to_path_buf());
        home.ensure_root().unwrap();
        let sid = "2645b710-862f-463a-9959-fe3c21690ffc";
        let mut entry = RegistryEntry::default();
        entry.name = "t-vendor-stop-opus".into();
        entry.harness = Some("claude".into());
        entry.short_id = "2645b710".into();
        entry.harness_session_id = Some(sid.into());
        entry.status = crate::AgentStatus::Exited;
        entry.node = Some("x-quiet-stop-guard".into());
        // The recorded worktree is gone, so even a gate regression cannot
        // reach a real subprocess: restore refuses before any spawn, and
        // the handoff refuses at owner resolution.
        entry.cwd = base
            .path()
            .join("worktree-gone")
            .to_string_lossy()
            .into_owned();
        let mut registry = crate::state::Registry::default();
        registry.entries.push(entry);
        crate::registry_store::seed_raw(
            &home.registry_json(),
            serde_json::to_vec(&registry).unwrap(),
        );
        let graph = crate::gc_sweep::graph_path(&home);
        crate::graph_store::locked_mutate(
            &graph,
            crate::graph_store::MutateInput {
                entries: vec![json!({
                    "id": "x-quiet-stop-guard",
                    "status": "in_progress",
                    "priority": "p2",
                    "created_at": "2026-11-10T19:00:00+00:00",
                })],
                canonical_path: None,
                base_version: crate::graph_store::base_version(&graph).unwrap(),
                plan_rungs: None,
            },
            crate::graph_store::DEFAULT_LOCK_TIMEOUT,
        )
        .unwrap();
        VENDOR_LISTING.with(|slot| {
            *slot.borrow_mut() = Some(crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("2645b710", Some("stopped")),
            ]))
        });
        let result = run(&home);
        VENDOR_LISTING.with(|slot| *slot.borrow_mut() = None);
        result.unwrap();
        let episode = home
            .root()
            .join("quiet-recovery")
            .join(format!("{sid}.json"));
        assert!(
            !episode.exists(),
            "the stopped worker left no recovery episode: {}",
            episode.display()
        );
        let events = std::fs::read_to_string(home.events_jsonl()).unwrap_or_default();
        assert!(
            !events.contains("quiet_worker_recovery") && !events.contains("quiet_worker_nudge"),
            "quiet recovery acted on a stopped worker: {events}"
        );
    }

    #[test]
    fn the_newest_live_ruling_parks_and_lifts_a_node_s_seat() {
        let ruling = |ts: &str, id: &str, text: &str| {
            json!({
                "_event_type": "operator_decision",
                "decision_id": id,
                "subject": "n-1",
                "ts": ts,
                "decision": text,
            })
        };
        let parked = vec![ruling(
            "2026-10-18T23:37:22Z",
            "d-1",
            "Park n-1 under the CI lane hold. Stop the worker.",
        )];
        assert!(parked_by_newest_ruling(&parked, "n-1"));
        assert!(
            !parked_by_newest_ruling(&parked, "n-2"),
            "another node's park ruling holds nothing here"
        );
        let mut lifted = parked.clone();
        lifted.push(ruling(
            "2026-10-19T01:00:00Z",
            "d-2",
            "Resume n-1: the lane hold lifted.",
        ));
        assert!(
            !parked_by_newest_ruling(&lifted, "n-1"),
            "the newest ruling governs, so a later resume lifts the park"
        );
        let lowercased = vec![ruling(
            "2026-10-18T23:37:22Z",
            "d-1",
            "park n-1 until the operator answers",
        )];
        assert!(parked_by_newest_ruling(&lowercased, "n-1"));
        assert!(!parked_by_newest_ruling(&[], "n-1"));
    }

    #[test]
    fn a_worker_aged_past_the_quiet_window_is_never_resumed_through_the_gates() {
        // The ladder alone resumes: dead, aged past the quiet window, no
        // fresh transcript work.
        let mut episode = Episode {
            activity: 100,
            ..Default::default()
        };
        let tail = Tail {
            activity: 100,
            ..Default::default()
        };
        assert_eq!(
            action(&mut episode, &tail, 4000, false, true),
            Action::Recover,
            "the ladder alone resumes a dead worker aged past the quiet window"
        );
        // Each gate outranks that ladder verdict. The lead's stop holds its
        // own worker, stamped per session, so a respawn keeps its own
        // recovery. A live park ruling holds the node's seat for whichever
        // worker sits in it.
        let sid = "11111111-1111-1111-1111-111111111111";
        let stopped: std::collections::HashSet<_> = [sid.to_string()].into_iter().collect();
        assert!(stopped.contains(sid));
        assert!(
            !stopped.contains("22222222-2222-2222-2222-222222222222"),
            "the stop evidence is stamped per session"
        );
        let park = json!({
            "_event_type": "operator_decision",
            "decision_id": "d-1",
            "subject": "n-1",
            "ts": "2026-10-18T23:37:22Z",
            "decision": "Park n-1 under the CI lane hold.",
        });
        assert!(parked_by_newest_ruling(&[park], "n-1"));
    }

    #[test]
    fn a_fleet_stop_holds_the_whole_arm_and_an_absent_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path().to_path_buf());
        let path = crate::fleet_incident::fleet_stop_path(&home);
        assert!(
            !crate::fleet_incident::read_at(&path).holds("spawns"),
            "an absent incident file reads clear: the arm runs"
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert!(
            crate::fleet_incident::read_at(&path).holds("spawns"),
            "an unreadable breaker holds, the way every admission door reads it"
        );
    }

    #[test]
    fn salvage_preserves_untracked_work_and_the_original_branch_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        assert_eq!(command(&["git", "init", "-q"], cwd).0, 0);
        assert_eq!(
            command(&["git", "config", "user.name", "Fixture"], cwd).0,
            0
        );
        assert_eq!(
            command(
                &["git", "config", "user.email", "fixture@example.test"],
                cwd
            )
            .0,
            0
        );
        std::fs::write(dir.path().join("tracked"), "base").unwrap();
        assert_eq!(command(&["git", "add", "tracked"], cwd).0, 0);
        assert_eq!(command(&["git", "commit", "-qm", "base"], cwd).0, 0);

        // A linked worktree off the canonical repo above: this is the
        // worker's own tree, AC7-HP's "transcript names its node worktree".
        let worker_dir = dir.path().join("worker-wt");
        assert_eq!(
            command(
                &[
                    "git",
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "feature/x",
                    worker_dir.to_str().unwrap(),
                ],
                cwd,
            )
            .0,
            0
        );
        let worker = worker_dir.to_str().unwrap();
        let head = command(&["git", "rev-parse", "HEAD"], worker).1;
        std::fs::write(worker_dir.join("tracked"), "staged").unwrap();
        assert_eq!(command(&["git", "add", "tracked"], worker).0, 0);
        std::fs::write(worker_dir.join("tracked"), "working").unwrap();
        std::fs::write(worker_dir.join(" new file"), "untracked").unwrap();

        let sid = "00000000-0000-4000-8000-000000000001";
        let home_root = tempfile::tempdir().unwrap();
        let claude_home = crate::claude_ask::ClaudeHome::at(home_root.path());
        let transcript_dir = claude_home
            .projects_dir()
            .join(crate::claude_ask::claude_cwd_slug(&worker_dir));
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(transcript_dir.join(format!("{sid}.jsonl")), "[]").unwrap();

        // The registry's recorded cwd is still the canonical repo; the
        // transcript resolves salvage to the worker's own linked worktree.
        let entry = RegistryEntry {
            cwd: cwd.into(),
            ..Default::default()
        };
        let receipt = salvage(&entry, sid, 1, &claude_home).unwrap();
        let branch = format!("refs/heads/recovery/{sid}-1");
        assert!(receipt.contains(&branch));
        assert_eq!(
            command(&["git", "show", &format!("{branch}:tracked")], worker).1,
            "working"
        );
        assert_eq!(
            command(&["git", "show", &format!("{branch}: new file")], worker).1,
            "untracked"
        );
        assert_eq!(command(&["git", "show", ":tracked"], worker).1, "staged");
        assert_eq!(command(&["git", "rev-parse", "HEAD"], worker).1, head);
        assert_eq!(
            std::fs::read_to_string(worker_dir.join("tracked")).unwrap(),
            "working"
        );

        // AC7-EDGE: a second row whose recorded cwd resolves to nothing but
        // itself (no transcript, no linked worktree it owns) must skip -
        // never mint a recovery ref from the canonical repo's own files.
        let canonical_only = RegistryEntry {
            cwd: cwd.into(),
            ..Default::default()
        };
        let other_sid = "00000000-0000-4000-8000-000000000002";
        let err = salvage(&canonical_only, other_sid, 1, &claude_home).unwrap_err();
        assert!(
            err.contains("salvage skipped") && err.contains("the main worktree"),
            "{err}"
        );
    }

    #[test]
    fn watch_shield_covers_the_bound_and_one_quiet_interval_past_it() {
        let watch = crate::watch_expiry::Watch {
            event_id: "e1".into(),
            seq: 1,
            session_id: "sess-a".into(),
            pr: Some(258),
            node: "x-1".into(),
            blocker: "ci".into(),
            task_id: None,
            reason: None,
            expires_at_ms: 1_000_000,
            ts_ms: 100_000,
        };
        assert!(watch_shielded(&watch, &[], watch.expires_at_ms - 1));
        assert!(watch_shielded(&watch, &[], watch.expires_at_ms));
        assert!(watch_shielded(
            &watch,
            &[],
            watch.expires_at_ms + WATCH_LINGER_MS - 1
        ));
        assert!(!watch_shielded(
            &watch,
            &[],
            watch.expires_at_ms + WATCH_LINGER_MS
        ));
    }

    #[test]
    fn a_declared_watch_shields_the_seat_whatever_runs_the_wait() {
        // The shield keys on the declared `<watching>` row, never on the
        // runner. A `fno do pr wait` watch and a background REST loop that
        // the worker declares write the same row, so both must shield.
        let now = 1_000_000;
        for runner_task in [None, Some("bash-rest-loop")] {
            let mut data = serde_json::json!({
                "harness": "claude",
                "pr": 149,
                "blocker": "ci",
                "reason": "ci",
                "node": "x-b153",
                "expires_at_ms": now + 15 * 60_000,
            });
            if let Some(task) = runner_task {
                data["task_id"] = serde_json::json!(task);
            }
            let evidence = vec![crate::watch_expiry::Evidence {
                event_id: "e1".into(),
                seq: 1,
                ts_ms: now - 60_000,
                kind: crate::watch_expiry::WATCH_IDLE.into(),
                session_id: Some("sess-a".into()),
                data,
            }];
            let watches = super::settle_watches(&evidence);
            assert_eq!(
                watches.len(),
                1,
                "a declared watch registers for {runner_task:?}"
            );
            assert!(
                watch_shielded(&watches[0].watch, &evidence, now + 60_000),
                "a live declared watch must shield the seat for {runner_task:?}"
            );
        }
    }

    #[test]
    fn the_expiry_wake_does_not_unshield_a_parked_session() {
        // The incident: a session parks on ci, the bound fires, the daemon
        // emits its wake row (which ends is_current_watch), and the worker's
        // next transcript write is still minutes out. The shield must hold
        // through the wake and one quiet interval past the bound.
        let evidence = vec![
            crate::watch_expiry::Evidence {
                event_id: "e1".into(),
                seq: 1,
                ts_ms: 100_000,
                kind: crate::watch_expiry::WATCH_IDLE.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({
                    "harness": "claude",
                    "expires_at_ms": 1_000_000,
                    "blocker": "ci",
                    "node": "x-1",
                }),
            },
            crate::watch_expiry::Evidence {
                event_id: "e2".into(),
                seq: 2,
                ts_ms: 1_000_050,
                kind: crate::watch_expiry::WAKE_EVENT.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({"watch_event_id": "e1"}),
            },
        ];
        let watch = &crate::codex_watch::settle_watches(&evidence)[0].watch;
        assert!(
            crate::watch_expiry::is_current_watch(watch, &evidence),
            "the wake row is not superseding evidence, so the watch stays current"
        );
        assert!(watch_shielded(watch, &evidence, 1_000_050));
        assert!(watch_shielded(
            watch,
            &evidence,
            watch.expires_at_ms + WATCH_LINGER_MS - 1
        ));
        assert!(!watch_shielded(
            watch,
            &evidence,
            watch.expires_at_ms + WATCH_LINGER_MS
        ));
    }

    #[test]
    fn a_superseded_watch_never_shields_the_seat() {
        // A watch a later idle row superseded (the worker re-armed or moved
        // on) releases the seat at once, however far its own bound still
        // sits in the future.
        let evidence = vec![crate::watch_expiry::Evidence {
            event_id: "e2".into(),
            seq: 2,
            ts_ms: 200_000,
            kind: crate::watch_expiry::WATCH_IDLE.into(),
            session_id: Some("sess-a".into()),
            data: serde_json::json!({
                "harness": "claude",
                "expires_at_ms": 1_100_000,
                "blocker": "ci",
                "node": "x-1",
            }),
        }];
        let superseded = crate::watch_expiry::Watch {
            event_id: "e1".into(),
            seq: 1,
            session_id: "sess-a".into(),
            pr: Some(258),
            node: "x-1".into(),
            blocker: "ci".into(),
            task_id: None,
            reason: None,
            expires_at_ms: 1_000_000,
            ts_ms: 100_000,
        };
        assert!(!crate::watch_expiry::is_current_watch(
            &superseded,
            &evidence
        ));
        assert!(!watch_shielded(&superseded, &evidence, 150_000));
    }

    #[test]
    fn an_early_wake_lights_the_shield_from_the_wake_not_the_bound() {
        // A ci watch settled ahead of its bound: the worker got its answer
        // early, so the linger runs from the wake's own stamp, not from the
        // bound it never reached. A worker that stalls after answering is
        // read on the same cadence as any other quiet worker.
        let evidence = vec![
            crate::watch_expiry::Evidence {
                event_id: "e1".into(),
                seq: 1,
                ts_ms: 100_000,
                kind: crate::watch_expiry::WATCH_IDLE.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({
                    "harness": "claude",
                    "expires_at_ms": 1_000_000,
                    "blocker": "ci",
                    "node": "x-1",
                }),
            },
            crate::watch_expiry::Evidence {
                event_id: "e2".into(),
                seq: 2,
                ts_ms: 500_000,
                kind: crate::watch_expiry::WAKE_EVENT.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({"watch_event_id": "e1"}),
            },
        ];
        let watch = &crate::codex_watch::settle_watches(&evidence)[0].watch;
        assert!(watch_shielded(watch, &evidence, 500_000));
        assert!(watch_shielded(
            watch,
            &evidence,
            500_000 + WATCH_LINGER_MS - 1
        ));
        assert!(!watch_shielded(watch, &evidence, 500_000 + WATCH_LINGER_MS));
    }

    #[test]
    fn only_wake_owners_other_than_the_ladder_park_the_seat() {
        assert!(route_parks_the_seat("timer"));
        assert!(route_parks_the_seat("off-session"));
        assert!(route_parks_the_seat("climbed"));
        assert!(route_parks_the_seat("question-page"));
        assert!(!route_parks_the_seat("ruled"));
        assert!(!route_parks_the_seat("in-session"));
        assert!(!route_parks_the_seat(""));
        assert!(
            !route_parks_the_seat("some-future-kind"),
            "an unknown route kind holds nothing"
        );
    }

    #[test]
    fn a_newest_help_wait_park_holds_the_ladder_until_activity_or_death() {
        let park = Some((2_000, "timer".to_string()));
        assert!(
            help_parked(park.as_ref(), 1_000, false),
            "a live route newer than the last work parks the seat"
        );
        assert!(
            help_parked(park.as_ref(), 2_000, false),
            "the help emission routes in the same second its transcript line lands"
        );
        assert!(
            !help_parked(park.as_ref(), 2_001, false),
            "activity past the row is the worker back, or the owner's wake landing"
        );
        assert!(
            !help_parked(park.as_ref(), 1_000, true),
            "a dead worker never parks: recovery stays the ladder's"
        );
        let ruled = Some((2_000, "ruled".to_string()));
        assert!(
            !help_parked(ruled.as_ref(), 1_000, false),
            "a settled escalation leaves the ordinary ladder"
        );
        assert!(!help_parked(None, 0, false), "no route row, no park");
    }

    /// One `help_route` row as the journal carries it.
    fn route_row(ts: &str, run: &str, route: &str) -> String {
        json!({"ts": ts, "type": "help_route", "run": run,
               "data": {"route": route}})
        .to_string()
    }

    #[test]
    fn the_newest_route_row_per_session_wins_over_journal_order() {
        let lines = [
            route_row("2026-01-01T00:05:00Z", "s1", "ruled"),
            route_row("2026-01-01T00:04:00Z", "s1", "off-session"),
            route_row("2026-01-01T00:04:00Z", "s2", "timer"),
            "not json at all".to_string(),
            route_row("2026-01-01T00:06:00Z", "s3", "ruled"),
        ];
        let stamp = |iso: &str| {
            chrono::DateTime::parse_from_rfc3339(iso)
                .unwrap()
                .timestamp()
        };
        let newest = fold_newest_help_routes(lines.iter().map(String::as_str));
        let s1 = "s1".to_string();
        assert_eq!(
            newest.get(&s1).expect("s1 joins the fold"),
            &(stamp("2026-01-01T00:05:00Z"), "ruled".to_string()),
            "the newest row wins over journal order"
        );
        let s2 = "s2".to_string();
        assert_eq!(
            newest.get(&s2).expect("s2 joins the fold"),
            &(stamp("2026-01-01T00:04:00Z"), "timer".to_string())
        );
        let s3 = "s3".to_string();
        assert_eq!(
            newest.get(&s3).expect("s3 joins the fold"),
            &(stamp("2026-01-01T00:06:00Z"), "ruled".to_string())
        );
        assert_eq!(
            newest.len(),
            3,
            "an unparsable line joins nothing, a session joins once"
        );
    }
}
