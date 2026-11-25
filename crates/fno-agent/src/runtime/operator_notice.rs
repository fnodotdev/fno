//! How a Rust caller reaches the operator, and when it should stay quiet.
//!
//! Three jobs share one module because they share one chokepoint: the
//! operator-notice send, which runs the notify dispatch owner's decision
//! table in-process (`crate::notify_dispatch`; the former `fno inbox notify`
//! child spawn retired with the in-process port), the state-change signal
//! store that keeps a sampler honest, and the notify_watch arm - the timer
//! that samples computed state and notifies only on a change. Every caller
//! into the chokepoint before this module was event-driven: something
//! happened inside a process, and that process posted. State that changes
//! with no event reached nobody, which is how main went red on 2026-09-06
//! and no instrument reported it for forty minutes.
//!
//! The arm samples the lead board queues and main CI's check runs, collapses
//! each signal on a token through the store, and emits one pointer notice per
//! changed signal. The token IS the state; the tick is only a sample. A
//! notice is a pointer to the durable queue, never a copy of it.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::org_board::s_str;

// ---------------------------------------------------------------------------
// The chokepoint: the dispatch, in-process
// ---------------------------------------------------------------------------

/// Fire-and-forget operator notice through the dispatch owner
/// ([`crate::notify_dispatch`]), run on a detached thread: `notify_transition`
/// runs inside a registry write and must not block on the toast or the mux
/// one-shot, so the table answers from the thread. Returns whether the send
/// left, so the signal store can refuse to commit state for a notice that
/// never left the machine; the dispatch always answers in-process today, so
/// the thread start is the send.
pub fn notify_operator(title: &str, body: &str, pointer: Option<&str>) -> bool {
    // Under cargo test, an unpinned binary resolves to the real `fno` in the
    // mux seam, and the in-process toast would fire for real (the 2026-10-01
    // leak: a test-seeded body reached the user as an OS alert). Refuse; a
    // test that pins `FNO_BIN` to a stub keeps the full dispatch.
    if cfg!(test) && std::env::var_os("FNO_BIN").is_none() {
        return false;
    }
    let title = title.to_string();
    let body = body.to_string();
    let pointer = pointer.map(str::to_string);
    std::thread::spawn(move || {
        let (code, err) = crate::notify_dispatch::send_notification(
            &title,
            &body,
            pointer.as_deref().unwrap_or(""),
        );
        if !err.is_empty() {
            eprintln!("fno-agent: operator notice: {err}");
        }
        let _ = code;
    });
    true
}

/// The same send, confirmed: the dispatch runs inline and the return is its
/// exit code, not the thread start. A pager whose `true` commits a dedupe
/// token (arm_watch, notify_watch) must call this, or a notice that died at
/// the gate reads as delivered and the episode goes quiet.
pub fn notify_operator_confirmed(title: &str, body: &str, pointer: Option<&str>) -> bool {
    // Same cargo-test refusal as [`notify_operator`]: unpinned under test
    // means the real `fno` in the mux seam, never a legal dispatch.
    if cfg!(test) && std::env::var_os("FNO_BIN").is_none() {
        return false;
    }
    let (code, err) = crate::notify_dispatch::send_notification(title, body, pointer.unwrap_or(""));
    if !err.is_empty() {
        eprintln!("fno-agent: operator notice: {err}");
    }
    code == 0
}

// ---------------------------------------------------------------------------
// The signal store
// ---------------------------------------------------------------------------

/// Whether a send went out, was already sent, or was held by the rate floor.
pub enum Verdict {
    Sent,
    Deduped,
    RateHeld,
    SendFailed,
}

pub(crate) fn notify_signals_path() -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_NOTIFY_SIGNALS").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| std::ffi::OsString::from("."));
    crate::state_layout::place(&PathBuf::from(home).join(".fno"), "notify-signals.json")
}

fn load_store(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_store(path: &Path, store: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // pid-suffixed temp + rename: a concurrent pass never reads a half file.
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string(store).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn age_seconds(ts: &str, now: u64) -> u64 {
    match crate::tick_ledger::parse_rfc3339_unix(ts) {
        Some(then) => now.saturating_sub(then),
        // Unreadable ts reads as old, never held forever.
        None => u64::MAX,
    }
}

/// Collapse one send on `token`. Deduped when the stored token equals it;
/// RateHeld when it changed inside the floor (held, not dropped - the token
/// stays unwritten, so the next pass reconsiders the same change); otherwise
/// the sender runs and state commits only on an accepted send.
pub fn notify_signal(
    key: &str,
    token: &str,
    title: &str,
    body: &str,
    pointer: Option<&str>,
    min_interval_s: u64,
) -> Verdict {
    notify_signal_via(
        &notify_signals_path(),
        now_unix(),
        min_interval_s,
        key,
        token,
        title,
        body,
        pointer,
        || notify_operator(title, body, pointer),
    )
}

/// The state machine behind `notify_signal`, with the send handed in - the
/// same seam the proven Python tests used at `send_notification`.
pub fn notify_signal_via(
    path: &Path,
    now: u64,
    min_interval_s: u64,
    key: &str,
    token: &str,
    _title: &str,
    _body: &str,
    _pointer: Option<&str>,
    send: impl FnOnce() -> bool,
) -> Verdict {
    let mut store = load_store(path);
    let entry = store.get(key).cloned();
    if let Some(entry) = &entry {
        if entry.get("token").and_then(Value::as_str) == Some(token) {
            return Verdict::Deduped;
        }
    }
    if let Some(entry) = &entry {
        if age_seconds(entry.get("ts").and_then(Value::as_str).unwrap_or(""), now) < min_interval_s
        {
            return Verdict::RateHeld;
        }
    }
    // Send first, commit only on an accepted send: a notice that never left
    // the machine must not leave state saying it did - the next pass retries.
    if !send() {
        return Verdict::SendFailed;
    }
    store.insert(
        key.to_string(),
        json!({"token": token, "ts": crate::events::now_rfc3339()}),
    );
    if let Err(e) = write_store(path, &store) {
        eprintln!("fno-agent: notify signal state write failed: {e}");
    }
    Verdict::Sent
}

/// Drop one signal's stored state, so the next change sends again. The empty
/// side of a signal (queue drained) calls this instead of notifying: silence
/// about nothing is the designed quiet.
pub fn forget(key: &str) {
    forget_at(&notify_signals_path(), key)
}

/// Record `token` under `key` and say whether it was new. False when the same
/// token is already stored, or when the store cannot be written: an act that
/// cannot record itself must not repeat every tick.
pub(crate) fn mark_once(path: &Path, key: &str, token: &str) -> bool {
    let mut store = load_store(path);
    if store
        .get(key)
        .and_then(|e| e.get("token"))
        .and_then(Value::as_str)
        == Some(token)
    {
        return false;
    }
    store.insert(
        key.to_string(),
        json!({"token": token, "ts": crate::events::now_rfc3339()}),
    );
    write_store(path, &store).is_ok()
}

pub fn forget_at(path: &Path, key: &str) {
    let mut store = load_store(path);
    if store.remove(key).is_some() {
        if let Err(e) = write_store(path, &store) {
            eprintln!("fno-agent: notify signal state write failed: {e}");
        }
    }
}

/// The seconds since the entry under `key` was first written, or `None` when
/// the store holds none. The team alarm's first-seen clock reads through
/// here: the store shapes stay private to this module.
pub(crate) fn first_seen_age_s(path: &Path, key: &str, now: u64) -> Option<u64> {
    let store = load_store(path);
    let ts = store.get(key)?.get("ts")?.as_str()?;
    Some(age_seconds(ts, now))
}

/// The stored keys carrying `prefix`, so a per-scope reader can act on what
/// it marked and forget the clocks of scopes that left its payload.
pub(crate) fn keys_with_prefix(path: &Path, prefix: &str) -> Vec<String> {
    load_store(path)
        .keys()
        .filter(|k| k.starts_with(prefix))
        .cloned()
        .collect()
}

/// The stored token under `key`, so a reader can act on what it marked. The
/// store shapes stay private to this module.
pub(crate) fn stored_token(path: &Path, key: &str) -> Option<String> {
    load_store(path)
        .get(key)
        .and_then(|e| e.get("token"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// The notify_watch arm
// ---------------------------------------------------------------------------

/// Board queues the arm subscribes to: key -> (pointer command, count label).
const BOARD_QUEUES: [(&str, &str, &str); 4] = [
    (
        "operator_question",
        "fno inbox outstanding",
        "open operator question(s)",
    ),
    (
        "mergeable_pr",
        "fno inbox board",
        "mergeable PR(s) with no live driver",
    ),
    (
        "undriven_pr",
        "fno inbox board",
        "PR(s) undriven across checks",
    ),
    (
        "failed_verdict",
        "fno inbox outstanding",
        "prove-it FAIL verdict(s) with no ruling",
    ),
];

/// A commit younger than this with an empty check-run list is still in its
/// launch window, not a "jobs never ran" state. Path-filtered CI stays empty
/// forever, so the arm waits out the window once and then reports it.
const EMPTY_RUNS_GRACE_S: u64 = 900;

/// One pass. `acted` counts notices that left the machine; `skip_reason`
/// names only whole-lane trouble - dedupe is the designed quiet, never a skip.
pub fn run_notify_watch(
    signals: &[String],
    roots: &[PathBuf],
    min_interval_s: u64,
) -> WatchOutcome {
    let mut acted: u64 = 0;
    let mut notes: Vec<String> = Vec::new();
    let mut skip: Option<String> = None;
    let path = notify_signals_path();
    let now = now_unix();

    // Board lane: ONE in-process read; a queue at zero is forgotten, a change
    // in a queue's row set is a notice, an unreadable queue skips the whole
    // lane - never reported as empty.
    let board_keys: Vec<&str> = BOARD_QUEUES
        .iter()
        .map(|(k, _, _)| *k)
        .filter(|k| signals.iter().any(|s| s == *k))
        .collect();
    if !board_keys.is_empty() {
        let board = crate::org_board::read_board(&crate::org_board::BoardOpts::default());
        let queues: Map<String, Value> = board
            .get("queues")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|q| {
                        let name = q.get("name").and_then(Value::as_str)?;
                        Some((name.to_string(), q.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let unreadable = board_keys.iter().any(|k| {
            queues
                .get(*k)
                .map(|q| q.get("status").and_then(Value::as_str) == Some("unreadable"))
                .unwrap_or(false)
        });
        if unreadable {
            skip = Some("board_unreadable".to_string());
            notes.push("board:unreadable".to_string());
        } else {
            for (key, pointer, label) in BOARD_QUEUES.iter() {
                if !board_keys.contains(key) {
                    continue;
                }
                let Some(queue) = queues.get(*key) else {
                    notes.push(format!("{key}:queue_absent"));
                    continue;
                };
                let count = queue.get("count").and_then(Value::as_i64).unwrap_or(0);
                if count <= 0 {
                    forget_at(&path, key);
                    notes.push(format!("{key}:clear"));
                    continue;
                }
                let rows = queue.get("rows").and_then(Value::as_array);
                let token = rows_token(rows);
                let body = format!("{count} {label}. {pointer}");
                let verdict = verdict_label(notify_signal_via(
                    &path,
                    now,
                    min_interval_s,
                    key,
                    &token,
                    &format!("operator: {label}"),
                    &body,
                    Some(pointer),
                    || {
                        notify_operator_confirmed(
                            &format!("operator: {label}"),
                            &body,
                            Some(pointer),
                        )
                    },
                ));
                acted += u64::from(verdict == "sent");
                notes.push(format!("{key}:{verdict}"));
                // The due lane rides the same queue read: no extra source.
                if *key == "operator_question" {
                    due_pass(
                        &path,
                        now,
                        min_interval_s,
                        rows,
                        &mut acted,
                        &mut notes,
                        &mut |t, m| notify_operator_confirmed(t, m, Some("fno inbox outstanding")),
                    );
                }
            }
        }
    }

    // Main CI lane: worst conclusion folded over the head commit's check
    // runs, with the run POPULATION in the token. A green that comes from
    // jobs not running must never read as a green from jobs passing - the
    // 2026-09-06 incident was a crates-only merge reading green because
    // cli-ci was path-filtered without crates/**.
    if signals.iter().any(|s| s == "main_ci") {
        if !gh_on_path() {
            if skip.is_none() {
                skip = Some("gh_absent".to_string());
            }
            notes.push("main_ci:gh_absent".to_string());
        } else {
            for root in roots {
                match main_ci_sample(root) {
                    None => notes.push(format!("main_ci:{}:unreadable", root.display())),
                    Some((key, token, body, pointer)) => {
                        let verdict = verdict_label(notify_signal_via(
                            &path,
                            now,
                            min_interval_s,
                            &key,
                            &token,
                            "main CI",
                            &body,
                            Some(&pointer),
                            || notify_operator_confirmed("main CI", &body, Some(&pointer)),
                        ));
                        acted += u64::from(verdict == "sent");
                        notes.push(format!("{key}:{verdict}"));
                    }
                }
            }
        }
    }

    // Release lane: the newest finished release.yml run per repo. A failure
    // pages once per run and mails the head lead; a pass clears the signal.
    if signals.iter().any(|s| s == "release_run") {
        for root in roots {
            acted += crate::release_watch::pass(root, &path, now, min_interval_s, &mut notes);
        }
    }

    WatchOutcome {
        acted,
        skip_reason: skip,
        detail: notes.join("; ").chars().take(200).collect(),
    }
}

pub struct WatchOutcome {
    pub acted: u64,
    pub skip_reason: Option<String>,
    pub detail: String,
}

pub(crate) fn verdict_label(v: Verdict) -> &'static str {
    match v {
        Verdict::Sent => "sent",
        Verdict::Deduped => "deduped",
        Verdict::RateHeld => "rate-held",
        Verdict::SendFailed => "send_failed",
    }
}

/// sha256 over the sorted row ids - membership change is state change.
fn rows_token(rows: Option<&Vec<Value>>) -> String {
    let mut ids: Vec<String> = rows
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|r| r.is_object())
        .map(|r| {
            r.get("id")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .or_else(|| {
                    r.get("number")
                        .and_then(Value::as_i64)
                        .map(|n| n.to_string())
                })
                .unwrap_or_default()
        })
        .collect();
    ids.sort();
    let mut h = Sha256::new();
    for id in &ids {
        h.update(id.as_bytes());
        h.update(b"\n");
    }
    format!("{:x}", h.finalize())
}

/// The open questions past their deadline: `(id, overdue days, question)`,
/// oldest first. A row the fold stamped `quiet` (snoozed, or before its
/// deadline) never enters; an unstamped row reads as due, which is also the
/// honest answer for a row filed before the stamp existed - it is old.
fn due_rows(rows: &[Value], now: i64) -> Vec<(String, u64, String)> {
    let mut due: Vec<(String, u64, String)> = rows
        .iter()
        .filter(|r| s_str(r, "quiet").is_none())
        .filter_map(|r| {
            let id = s_str(r, "id")?.to_string();
            let ts = s_str(r, "ts")?.to_string();
            let question = s_str(r, "question")?.to_string();
            match crate::attention::quiet_from(&ts, None, None, now) {
                crate::attention::Quiet::Due { epoch } => Some((id, epoch, question)),
                _ => None,
            }
        })
        .collect();
    due.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    due
}

/// sha256 over the `(id, window)` pairs - the set is the state, and the
/// window count inside it makes each 24h window a new state, which is the
/// whole re-delivery cadence.
fn due_token(due: &[(String, u64, String)]) -> String {
    let mut h = Sha256::new();
    h.update(due.len().to_le_bytes());
    for (id, epoch, _) in due {
        h.update(id.as_bytes());
        h.update(epoch.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}

/// The notice body: the count, the three oldest by id with how long each
/// has waited, and the answer pointer. The age is days since the ask, a
/// number true for every question, never an overdue figure the lane cannot
/// know for a question filed with its own deadline. The durable queue holds
/// the rest.
fn due_body(due: &[(String, u64, String)]) -> String {
    let mut body = format!("{} question(s) overdue", due.len());
    for (id, epoch, question) in due.iter().take(3) {
        let excerpt: String = question.chars().take(60).collect();
        body.push_str(&format!("; {id} asked {}d ago \"{excerpt}\"", epoch + 1));
    }
    let hidden = due.len().saturating_sub(3);
    if hidden > 0 {
        body.push_str(&format!("; and {hidden} more"));
    }
    body.push_str(". Answer: fno inbox outstanding");
    body
}

/// The signal-store key of the due lane, at module scope so the tests can
/// assert the forget without naming the string twice.
const DUE_KEY: &str = "operator_question_due";

/// The due re-delivery leg of the board lane. A question past its deadline
/// sits silent in the row-set lane above (its rows never change), which is
/// how filed asks vanished for days with no ruling. One notice per
/// 24h window past the deadline, through the same dedupe state machine as
/// every lane - the window count in the token is the whole cadence. An
/// empty due set (or an unreadable one) forgets the key like any cleared
/// lane. The rows arrive read: the caller already lifted them off the
/// queue for the row-set token.
fn due_pass(
    path: &Path,
    now: u64,
    min_interval_s: u64,
    rows: Option<&Vec<Value>>,
    acted: &mut u64,
    notes: &mut Vec<String>,
    send: &mut dyn FnMut(&str, &str) -> bool,
) {
    let due = rows.map(|r| due_rows(r, now as i64)).unwrap_or_default();
    if due.is_empty() {
        forget_at(path, DUE_KEY);
        notes.push(format!("{DUE_KEY}:clear"));
        return;
    }
    let token = due_token(&due);
    let body = due_body(&due);
    const TITLE: &str = "operator: overdue questions";
    const POINTER: &str = "fno inbox outstanding";
    let verdict = verdict_label(notify_signal_via(
        path,
        now,
        min_interval_s,
        DUE_KEY,
        &token,
        TITLE,
        &body,
        Some(POINTER),
        || send(TITLE, &body),
    ));
    *acted += u64::from(verdict == "sent");
    notes.push(format!("{DUE_KEY}:{verdict}"));
}

/// Fold the shared reader's classified main-CI conclusion into
/// `(population, verdict)`, or None when there is no state to report: a
/// pending read is not a state (notifying on it would spam), and an empty
/// list inside the launch window is the same not-a-state. An empty list past
/// the grace is its own state and never folds into success.
fn fold_check_runs(
    conclusion: crate::loopcheck::CiConclusion,
    population: usize,
    head_age_s: Option<u64>,
) -> Option<(usize, String)> {
    if population == 0 {
        let old_enough = head_age_s
            .map(|age| age >= EMPTY_RUNS_GRACE_S)
            .unwrap_or(true);
        return if old_enough {
            Some((0, "none".to_string()))
        } else {
            None
        };
    }
    match conclusion {
        crate::loopcheck::CiConclusion::Failure(_) => Some((population, "failure".to_string())),
        crate::loopcheck::CiConclusion::Success => Some((population, "success".to_string())),
        _ => None,
    }
}

/// The sample body: a failure names its checks, so a run that failed before
/// minting a job reads by its workflow path and not as a count alone.
fn main_ci_body(slug: &str, population: usize, verdict: &str, failing: &[String]) -> String {
    if population == 0 {
        return format!("main CI on {slug}: no check runs (population 0); jobs may not have run.");
    }
    if verdict == "failure" {
        let names = if failing.is_empty() {
            "unnamed".to_string()
        } else {
            failing.join(", ")
        };
        return format!("main CI on {slug}: {verdict} ({population} check run(s)): {names}.");
    }
    format!("main CI on {slug}: {verdict} ({population} check run(s)).")
}

fn gh_on_path() -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| dir.join("gh").is_file())
}

/// One repo's main-CI sample: `(signal key, token, body)`, or None when the
/// world cannot be read or has no state yet. The git legs run INSIDE the repo
/// (the proven Python leg's first draft took no cwd and every sample died on
/// it); the https slug drops the host, or the gh api path is wrong.
fn main_ci_sample(root: &Path) -> Option<(String, String, String, String)> {
    let url = run_captured(
        "git",
        &["remote", "get-url", "origin"],
        root,
        Duration::from_secs(10),
    )?;
    let slug = slug_from_remote(url.trim());
    if slug.is_empty() {
        return None;
    }
    let branch_out = run_captured(
        "git",
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
        root,
        Duration::from_secs(10),
    )?;
    let branch = branch_out
        .trim()
        .strip_prefix("origin/")
        .unwrap_or(branch_out.trim())
        .to_string();
    let branch = if branch.is_empty() {
        "main".to_string()
    } else {
        branch
    };
    let ref_enc = percent_encode_ref(&branch);
    // The branch head sha, so the shared reader reads the commit and not the
    // ref name (read_checks_rows pins a sha against check runs).
    let sha_out = run_captured(
        "gh",
        &[
            "api",
            &format!("repos/{slug}/commits/{ref_enc}"),
            "--jq",
            ".sha",
        ],
        root,
        Duration::from_secs(30),
    )?;
    let sha = sha_out.trim().to_string();
    let rows = crate::pr_push::read_checks_rows("gh", root, &sha).ok()?;
    let population = rows.len();
    let head_age_s = if population == 0 {
        Some(head_commit_age_s(root, &slug, &ref_enc)?)
    } else {
        None
    };
    let failing: Vec<String> = rows
        .iter()
        .filter(|r| {
            matches!(
                r.get("bucket").and_then(Value::as_str),
                Some("fail") | Some("cancel")
            )
        })
        .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let conclusion = crate::loopcheck::classify_checks_payload(&Value::Array(rows))
        .ok()?
        .0;
    let (population, verdict) = fold_check_runs(conclusion, population, head_age_s)?;
    let body = main_ci_body(&slug, population, &verdict, &failing);
    Some((
        format!("main_ci:{slug}"),
        format!("{population}:{verdict}"),
        body,
        format!("https://github.com/{slug}/actions"),
    ))
}

/// Unix age of the head commit's committer date; an unreadable date reads as
/// old (the empty state reports rather than hiding behind a parse failure).
fn head_commit_age_s(root: &Path, slug: &str, ref_enc: &str) -> Option<u64> {
    let out = run_captured(
        "gh",
        &["api", &format!("repos/{slug}/commits/{ref_enc}")],
        root,
        Duration::from_secs(30),
    )?;
    let head: Value = serde_json::from_str(out.trim()).ok()?;
    let date = head
        .pointer("/commit/committer/date")
        .and_then(Value::as_str)
        // Some histories carry only an author date.
        .or_else(|| head.pointer("/commit/author/date").and_then(Value::as_str))?;
    let then = crate::tick_ledger::parse_rfc3339_unix(&date)?;
    Some(now_unix().saturating_sub(then))
}

/// `owner/repo` from an https or scp-form git remote url, empty when unclear.
pub(crate) fn slug_from_remote(url: &str) -> String {
    let mut u = url.trim().trim_end_matches(".git").trim().to_string();
    if let Some(pos) = u.find("://") {
        u = u[pos + 3..].to_string();
        u = match u.find('/') {
            Some(p) => u[p + 1..].to_string(),
            None => String::new(),
        };
    } else {
        u = match u.find(':') {
            Some(p) => u[p + 1..].to_string(),
            None => String::new(),
        };
    }
    let u = u.trim_matches('/');
    if u.matches('/').count() == 1 {
        u.to_string()
    } else {
        String::new()
    }
}

/// Percent-encode a ref for a gh api path: unreserved bytes pass, everything
/// else rides its UTF-8 percent triple (a branch named `feature/x` is one
/// path segment, not two).
fn percent_encode_ref(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Run one capture command inside `cwd`, bounded by `timeout`, stdout
/// drained on its own thread so a chatty child cannot deadlock the wait.
/// Returns stdout regardless of exit status; the caller judges the content.
pub(crate) fn run_captured(
    bin: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    // The fleet budget admits before the spawn: a refused gh read takes the
    // caller's unreadable branch instead of sending the request.
    if crate::gh_budget::admit_spawn(OsStr::new(bin), args).is_some() {
        return None;
    }
    let mut child = crate::bounded_spawn::spawn_bounded(OsStr::new(bin), args, cwd).ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();

        let _ = std::io::Read::read_to_string(&mut stdout, &mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Some(reader.join().unwrap_or_default()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// `fno-agent notify-watch --json [--root PATH]...`: run one pass, print the
/// receipt, exit 0 - an honest skip is the tick row's business, never an
/// error. Not a routable `fno agent` command (matched with `==` beside `board`),
/// so the routable-command parity guard sees no new advertised command.
pub fn run_notify_watch_command(args: &[String]) -> i32 {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" | "-J" => {}
            "--root" => match it.next() {
                Some(r) => roots.push(PathBuf::from(r)),
                None => {
                    eprintln!("fno-agent notify-watch: --root needs a path");
                    return 2;
                }
            },
            other => {
                eprintln!("fno-agent notify-watch: unknown flag {other}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let signals = crate::agents_config::notify_signals(&cwd);
    if signals.is_empty() {
        println!(
            "{}",
            json!({"acted": 0, "skip_reason": "notify_off", "detail": "[notify] signals empty"})
        );
        return 0;
    }
    let floor = crate::agents_config::notify_min_interval_s(&cwd);
    let out = run_notify_watch(&signals, &roots, floor);
    println!(
        "{}",
        json!({"acted": out.acted, "skip_reason": out.skip_reason, "detail": out.detail})
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::test_env_lock;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-operator-notice-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p.set_extension("json");
        p
    }

    fn sent_once(counter: &mut u32) -> impl FnOnce() -> bool + '_ {
        move || {
            *counter += 1;
            true
        }
    }

    #[test]
    fn same_token_dedupes_and_sends_nothing() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_path("dedupe");
        let mut sends: u32 = 0;
        assert!(matches!(
            notify_signal_via(
                &path,
                1_000,
                300,
                "k",
                "t1",
                "T",
                "B",
                None,
                sent_once(&mut sends)
            ),
            Verdict::Sent
        ));
        assert_eq!(sends, 1);
        assert!(matches!(
            notify_signal_via(
                &path,
                1_100,
                300,
                "k",
                "t1",
                "T",
                "B",
                None,
                sent_once(&mut sends)
            ),
            Verdict::Deduped
        ));
        assert_eq!(sends, 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn changed_token_inside_floor_is_held_not_dropped() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_path("held");
        let mut sends: u32 = 0;
        notify_signal_via(
            &path,
            1_000,
            3_600,
            "k",
            "t1",
            "T",
            "B",
            None,
            sent_once(&mut sends),
        );
        assert!(matches!(
            notify_signal_via(
                &path,
                1_060,
                3_600,
                "k",
                "t2",
                "T",
                "B",
                None,
                sent_once(&mut sends)
            ),
            Verdict::RateHeld
        ));
        assert_eq!(sends, 1, "held, not sent");
        // The token was NOT written: the next pass after the floor reconsiders t2.
        let store = load_store(&path);
        assert_eq!(store["k"]["token"], "t1");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn failed_send_rolls_back_so_the_next_pass_retries() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_path("rollback");
        assert!(matches!(
            notify_signal_via(&path, 1_000, 300, "k", "t1", "T", "B", None, || false),
            Verdict::SendFailed
        ));
        let store = load_store(&path);
        assert!(
            store.get("k").is_none(),
            "no state for a notice that never left"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_failure_body_names_the_failing_workflow_path() {
        let body = main_ci_body(
            "o/r",
            11,
            "failure",
            &[".github/workflows/cli-ci.yml".to_string()],
        );
        assert!(
            body.contains(".github/workflows/cli-ci.yml"),
            "body must name the file: {body}"
        );
    }

    #[test]
    fn percent_encode_ref_keeps_a_slashed_branch_one_segment() {
        assert_eq!(percent_encode_ref("main"), "main");
        assert_eq!(percent_encode_ref("feature/x"), "feature%2Fx");
        assert_eq!(percent_encode_ref("rel-1.2_3~4"), "rel-1.2_3~4");
    }

    #[test]
    fn both_senders_refuse_under_unit_tests_without_a_pin() {
        // The 2026-10-01 leak class: under cargo test, an unpinned FNO_BIN
        // resolves to the real `fno` in the mux seam, and the in-process
        // toast would reach the operator's screen. Both senders refuse.
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let bin = std::env::var_os("FNO_BIN");
        std::env::remove_var("FNO_BIN");
        let fired = notify_operator("T", "B", None);
        let confirmed = notify_operator_confirmed("T", "B", None);
        match bin {
            Some(v) => std::env::set_var("FNO_BIN", v),
            None => std::env::remove_var("FNO_BIN"),
        }
        assert!(!fired);
        assert!(!confirmed);
    }

    fn ts_rfc3339(unix: i64) -> String {
        chrono::DateTime::from_timestamp(unix, 0)
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default()
    }

    #[test]
    fn unstamped_rows_read_due_and_quiet_stamped_rows_do_not() {
        let now = 1_000_000;
        let old = ts_rfc3339(now - 3 * 86_400);
        let fresh = ts_rfc3339(now - 3_600);
        let rows = vec![
            json!({"id": "D2", "ts": old, "question": "snoozed", "quiet": "snoozed until you reply"}),
            json!({"id": "D1", "ts": old, "question": "old one", "quiet": Value::Null}),
            json!({"id": "D4", "ts": old, "question": "unstamped reads due"}),
            json!({"id": "D3", "ts": fresh, "question": "fresh", "quiet": "quiet until tomorrow"}),
        ];
        let due = due_rows(&rows, now);
        let ids: Vec<&str> = due.iter().map(|(id, _, _)| id.as_str()).collect();
        // Oldest first, then id for determinism: D1 and D4 share the window.
        assert_eq!(ids, vec!["D1", "D4"]);
    }

    #[test]
    fn due_token_changes_when_a_window_increments() {
        let due = vec![("D3634".to_string(), 2, "q".to_string())];
        let t2 = due_token(&due);
        let bumped: Vec<(String, u64, String)> = due
            .iter()
            .map(|(id, e, q)| (id.clone(), e + 1, q.clone()))
            .collect();
        assert_ne!(t2, due_token(&bumped));
        assert_eq!(t2, due_token(&due));
    }

    #[test]
    fn due_body_names_oldest_and_hides_the_rest() {
        let due = vec![
            ("D3634".to_string(), 2, "first question".to_string()),
            ("D3636".to_string(), 1, "second question".to_string()),
        ];
        let body = due_body(&due);
        assert!(body.starts_with("2 question(s) overdue"), "{body}");
        assert!(
            body.contains("D3634 asked 3d ago \"first question\""),
            "{body}"
        );
        assert!(
            body.contains("D3636 asked 2d ago \"second question\""),
            "{body}"
        );
        assert!(body.ends_with("Answer: fno inbox outstanding"), "{body}");
        let hidden = due_body(&[
            ("A".to_string(), 1, "q".to_string()),
            ("B".to_string(), 1, "q".to_string()),
            ("C".to_string(), 1, "q".to_string()),
            ("D".to_string(), 1, "q".to_string()),
        ]);
        assert!(hidden.contains("and 1 more"), "{hidden}");
    }

    #[test]
    fn due_pass_sends_three_phase_contract() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_path("due-pass");
        let now = 1_000_000u64;
        let row = || json!({"id": "D3634", "ts": ts_rfc3339(now as i64 - 3 * 86_400), "question": "vanished ask"});
        let sends = std::cell::Cell::new(0u32);
        let mut send = |_: &str, _: &str| {
            sends.set(sends.get() + 1);
            true
        };
        let mut acted = 0u64;
        let mut notes: Vec<String> = Vec::new();
        let rows = vec![row()];
        let empty: Vec<Value> = Vec::new();

        // Day one: the first overdue window sends.
        due_pass(
            &path,
            now,
            0,
            Some(&rows),
            &mut acted,
            &mut notes,
            &mut send,
        );
        assert_eq!(sends.get(), 1, "first window sends");
        assert!(notes.contains(&format!("{DUE_KEY}:sent")), "{notes:?}");

        // A re-read of the same state stays quiet.
        due_pass(
            &path,
            now,
            0,
            Some(&rows),
            &mut acted,
            &mut notes,
            &mut send,
        );
        assert_eq!(sends.get(), 1, "same state dedupes");
        assert!(notes.contains(&format!("{DUE_KEY}:deduped")), "{notes:?}");

        // The next window is new state: it re-fires. This is the ladder.
        due_pass(
            &path,
            now + 86_400,
            0,
            Some(&rows),
            &mut acted,
            &mut notes,
            &mut send,
        );
        assert_eq!(sends.get(), 2, "next window re-fires");
        assert!(notes.contains(&format!("{DUE_KEY}:sent")), "{notes:?}");

        // An answer empties the set: the clear note lands and the key is
        // forgotten, so a future due set starts from a clean state.
        due_pass(
            &path,
            now + 86_400,
            0,
            Some(&empty),
            &mut acted,
            &mut notes,
            &mut send,
        );
        assert_eq!(sends.get(), 2);
        assert!(notes.contains(&format!("{DUE_KEY}:clear")), "{notes:?}");
        assert!(load_store(&path).get(DUE_KEY).is_none(), "key forgotten");
        std::fs::remove_file(&path).ok();
    }
}
