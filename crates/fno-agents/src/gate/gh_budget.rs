//! One machine-wide GitHub request budget that every gh call is admitted
//! against.
//!
//! GitHub's secondary limit counts request RATE and concurrency across every
//! session on this machine's account, and before this module nothing added
//! the fleet's schedules up: each guard in the tree reacted after a refusal
//! and kept its own memory of it, so stand-downs kept spending probe
//! requests while GitHub was refusing. This module answers one question
//! BEFORE a request leaves - "may this GitHub request go out now?" - from
//! one JSON ledger every caller shares.
//!
//! Law D3210: fno never refuses a GitHub call on its own guess. The only
//! holds here are grounded in GitHub's own word - a backoff this token's
//! real refusal opened, and the core gate's own rate-limit reading. The
//! machine and poller caps count telemetry; an over-cap call goes out and
//! the ledger records the spend.
//!
//! The ledger lives in the machine locks folder (`machine_locks_dir()`,
//! joined with `github-request-budget.json`), config-independent on
//! purpose: a budget that protects the machine must not move with a
//! project's config. Each read-modify-write takes a `libc::flock` on
//! a `.lock` sidecar and writes through a temp file + rename, so a reader
//! never sees a torn file.
//!
//! Fail-open rule: an unreadable or corrupt ledger admits and says so in the
//! receipt. This budget protects the fleet; it is not a stop. An all-stop is
//! the operator's breaker (`fno agents incident`), and a disk fault must not
//! refuse every gh call on the machine.
//!
//! Command surface: the ops ride the EXISTING `fleet-incident` action as its
//! `gh-budget` argument (law d-fe66560a allows no new client action):
//! `fno-agents fleet-incident gh-budget <<< '{"op":"admit","argv":[...]}'`.
//! One JSON payload on stdin (the `rust_binary.command_call` door convention),
//! one JSON object on stdout. All three ops exit 0; the refusal is carried
//! in the payload, never in the exit code, because `command_call` raises on a
//! nonzero exit.
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::io::{IsTerminal, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The telemetry reference line: half of GitHub's advertised 900
/// points/minute REST secondary limit. It caps nothing (law D3210); `status`
/// prints the window's spend beside it so a runaway is visible before
/// GitHub refuses.
pub const DEFAULT_POINTS_PER_MIN: u32 = 450;
/// The telemetry reference line for one poll-class caller's rolling-minute
/// spend. The 2026-11-03 outage ran a wedged unlabeled poller at 4.5 Hz for
/// 4.3 hours (12,410 rows); the ledger keeps that visibility, and GitHub's
/// own refusals are the bound (law D3210).
pub const POLLER_CAP_PER_MIN: u32 = 120;
const WINDOW_MS: i64 = 60_000;
const HALF_OPEN_MS: i64 = 30_000;
const BASE_BACKOFF_MS: i64 = 60_000;
/// The flock wait bound: LOCK_NB retries, not a blocking LOCK_EX. The
/// critical section after the shared-hold hoist is a ledger read-write, so
/// 20 x 100ms covers a normal handoff while naming the wait instead of
/// queueing the merge path behind a stalled holder.
const LOCK_RETRY_MAX_TRIES: u32 = 20;
const LOCK_RETRY_SLEEP_MS: u64 = 100;
/// How old the stored core reading may be and still vouch for the bucket.
/// The bucket is account-wide, so a reading goes stale the moment the OTHER
/// machine (or any plain read whose headers nobody sees) spends it: the
/// 2026-10-25 outage ran five hours on a reading stamped early in the
/// window while the true count sat at 0. A quarter of the one-hour window
/// bounds how far a positive reading may bless refusals and downgrades.
pub(crate) const CORE_READING_FRESH_MS: i64 = 900_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerState {
    Ok,
    Absent,
    Unreadable,
}

fn ledger_word(state: LedgerState) -> &'static str {
    match state {
        LedgerState::Ok => "ok",
        LedgerState::Absent => "absent",
        LedgerState::Unreadable => "unreadable",
    }
}

#[derive(Debug, Clone)]
struct Ledger {
    stamps: Vec<(i64, u32)>,
    /// Per-caller spend stamps `(ts_ms, caller label, points)`, pruned on
    /// write like `stamps`. The poller cap sums one label's window.
    caller_stamps: Vec<(i64, String, u32)>,
    backoff_until_ms: i64,
    backoff_step: i64,
    /// The GitHub response kind of the live hold's opening refusal:
    /// secondary, core, abuse, or unknown. Older ledgers read unknown.
    refusal_kind: String,
    /// 0 means "never refused"; a real refusal stamp is a real epoch ms.
    last_refusal_ms: i64,
    /// 0 means "no half-open probe yet"; the first probe during a backoff
    /// always passes so recovery gets noticed instead of waited out.
    last_half_open_ms: i64,
    /// GitHub's own account-wide core reading: -1 unknown, 0 exhausted.
    /// Writers pass what the conditional door, the classify probe or the
    /// loop gate observed; a later reset replaces the stored value.
    core_remaining: i64,
    /// The epoch ms the stored reading's bucket resets at; 0 unknown.
    core_reset_ms: i64,
    /// When the stored reading was observed; 0 never.
    core_seen_ms: i64,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            stamps: Vec::new(),
            caller_stamps: Vec::new(),
            backoff_until_ms: 0,
            backoff_step: 0,
            refusal_kind: "unknown".to_string(),
            last_refusal_ms: 0,
            last_half_open_ms: 0,
            // Unknown, never 0: an absent or unreadable ledger must not
            // read as an exhausted account during outage diagnosis. The
            // admit gate stays inert anyway (reset 0 is never in the
            // future); this keeps `status` honest.
            core_remaining: -1,
            core_reset_ms: 0,
            core_seen_ms: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub admitted: bool,
    pub cause: Option<&'static str>,
    pub retry_after_s: i64,
    pub points_60s: i64,
    pub cap: u32,
    pub ledger: LedgerState,
    pub refusal: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub points_60s: i64,
    pub cap: u32,
    pub backoff_remaining_s: i64,
    pub ledger: LedgerState,
    /// The response kind stamped on the hold's opening refusal.
    pub refusal_kind: String,
    /// GitHub's account-wide core reading: -1 unknown.
    pub core_remaining: i64,
    /// Seconds until the stored core reading resets; 0 when unknown or past.
    pub core_reset_in_s: i64,
    /// Seconds since the stored core reading was observed; -1 never. A
    /// positive reading this old is a memory of a bucket other spenders
    /// have long since drained, never a present answer.
    pub core_seen_age_s: i64,
    /// Whether the stored reading is young enough to vouch for the bucket
    /// ([`CORE_READING_FRESH_MS`]). A refusal classifier consults this
    /// before trusting `core_remaining`.
    pub core_reading_fresh: bool,
    /// The active reserve; 0 disables the reserve refusals.
    pub core_reserve: u32,
}

pub fn ledger_path() -> PathBuf {
    // A HOME-unset context (cron, launchd) must not scatter the ledger into
    // whatever cwd invoked gh; the temp dir keeps admission working and the
    // file out of sight until HOME comes back.
    crate::agents_config::machine_locks_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("fno-locks"))
        .join("github-request-budget.json")
}

pub fn cap_from_env() -> u32 {
    std::env::var("FNO_GH_BUDGET_POINTS_PER_MIN")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_POINTS_PER_MIN)
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Port of `fno.pr._quota.command_args`: drop gh-wide options so equivalent
/// command spellings share one cost.
pub(crate) fn command_args(args: &[String]) -> Vec<String> {
    fn strip(tokens: &[String]) -> Vec<String> {
        let mut rest = tokens.to_vec();
        while let Some(first) = rest.first() {
            if first == "-R" || first == "--repo" || first == "--hostname" {
                if rest.len() < 2 {
                    return Vec::new();
                }
                rest.drain(0..2);
            } else if (first.starts_with("-R") && first.len() > 2)
                || first.starts_with("--repo=")
                || first.starts_with("--hostname=")
            {
                rest.remove(0);
            } else {
                break;
            }
        }
        rest
    }
    let normalized = strip(args);
    if normalized.first().map(String::as_str) == Some("pr") {
        let mut out = vec!["pr".to_string()];
        out.extend(strip(&normalized[1..]));
        return out;
    }
    normalized
}

fn rest_write(cmd: &[&str]) -> bool {
    for (i, tok) in cmd.iter().enumerate() {
        let tok = *tok;
        let method = if tok == "-X" || tok == "--method" {
            cmd.get(i + 1).copied()
        } else if let Some(v) = tok.strip_prefix("--method=") {
            Some(v)
        } else if tok.len() > 2 && tok.starts_with("-X") {
            Some(&tok[2..])
        } else {
            None
        };
        if let Some(m) = method {
            return matches!(
                m.to_ascii_uppercase().as_str(),
                "POST" | "PATCH" | "PUT" | "DELETE"
            );
        }
    }
    false
}

/// Whether an `api graphql` argv carries a mutation: the query rides either
/// as the positional value after `graphql` or as `-f query=...`.
fn graphql_mutation(cmd: &[&str]) -> bool {
    let Some(pos) = cmd.iter().position(|t| *t == "graphql") else {
        return false;
    };
    let mut query: Option<String> = None;
    let mut i = pos + 1;
    while i < cmd.len() {
        let tok = cmd[i];
        let inline = tok
            .strip_prefix("--field=")
            .or_else(|| tok.strip_prefix("--raw-field="));
        if let Some(v) = inline {
            if let Some(val) = v.strip_prefix("query=") {
                query = Some(val.to_string());
            }
        } else if tok == "-f" || tok == "-F" || tok == "--field" || tok == "--raw-field" {
            if let Some(next) = cmd.get(i + 1) {
                if let Some(val) = next.strip_prefix("query=") {
                    query = Some(val.to_string());
                }
            }
            i += 1;
        } else if !tok.starts_with('-') {
            query = Some(tok.to_string());
        }
        i += 1;
    }
    query.map(is_mutation_query).unwrap_or(false)
}

/// An unreadable query (a literal `@file` argv, gh loads it client-side)
/// fails toward the WRITE cost: an unprovable read is charged as a mutation,
/// never the reverse.
fn is_mutation_query(q: String) -> bool {
    if q.trim_start().starts_with('@') {
        return true;
    }
    q.trim_start().starts_with("mutation")
}

/// The request cost of one gh argv, in GitHub's advertised points: a GET or
/// a GraphQL query is 1, a content-creating request is 5, and the exempt
/// local/exempt-endpoint shapes are 0.
pub fn points_for(argv: &[String]) -> u32 {
    // ponytail: `--paginate` sends one request per page but is charged once
    // here; if pagination bursts trip the real limit, charge per-page via a
    // last-page estimate instead of raising the cap.
    let normalized = command_args(argv);
    let cmd: Vec<&str> = normalized.iter().map(String::as_str).collect();
    match cmd.first().copied() {
        Some("--version" | "version" | "help" | "--help" | "completion" | "config" | "alias") => 0,
        Some("auth") => match cmd.get(1).copied() {
            // `auth status` is the login read the shared hold keys on;
            // charging it would re-enter admit from the hold read and
            // recurse (gh_login). At most one validation call per hour
            // behind the login cache.
            Some("status") => 0,
            _ => 1,
        },
        Some("api") => {
            if cmd.get(1) == Some(&"rate_limit") {
                return 0; // exempt from the primary limit; the budget need not count it
            }
            if graphql_mutation(&cmd) || rest_write(&cmd) {
                5
            } else {
                1
            }
        }
        Some("pr") => match cmd.get(1).copied() {
            Some("create" | "merge" | "edit" | "comment" | "review" | "close" | "ready") => 5,
            _ => 1,
        },
        Some("issue") => match cmd.get(1).copied() {
            Some("create" | "comment") => 5,
            _ => 1,
        },
        Some("release") => match cmd.get(1).copied() {
            Some("create") => 5,
            _ => 1,
        },
        _ => 1,
    }
}

fn window_points(ledger: &Ledger, now: i64) -> i64 {
    ledger
        .stamps
        .iter()
        .filter(|(t, _)| now - *t < WINDOW_MS)
        .map(|(_, p)| *p as i64)
        .sum()
}

pub(crate) fn lock_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", path.display()))
}

/// flock held for the scope of one read-modify-write, following
/// `loop_lead::bump_respawn_count`. Shared with the pr-park store so the
/// daemon sweep and the watcher tick serialize on the same `.lock` file.
#[derive(Debug)]
pub(crate) struct FileLock {
    handle: std::fs::File,
}

impl FileLock {
    /// The only way to take the lock: Err when the lock file cannot be
    /// created or flocked (a read-only state directory, or a holder that
    /// keeps it past the retry bound). Each caller names its own degrade,
    /// and none aborts the process - the module's fail-open rule.
    ///
    /// The flock is LOCK_NB with a short retry, never a blocking LOCK_EX: a
    /// blocking wait queued every gh shim call behind the lock holder, and
    /// one holder reading the claims store inside its critical section made
    /// the queue minutes deep (measured 2026-10-24: 25s per call, 13
    /// holders). The bound keeps a brief handoff while naming the wait in
    /// the error, so a starved caller degrades lock-free instead of
    /// stalling the merge path.
    pub(crate) fn try_acquire(path: &Path) -> Result<FileLock, String> {
        Self::try_acquire_with(path, LOCK_RETRY_MAX_TRIES, LOCK_RETRY_SLEEP_MS)
    }

    /// The parametric form, tries and sleep handed in so a test drives the
    /// bound without waiting out the real one.
    pub(crate) fn try_acquire_with(
        path: &Path,
        max_tries: u32,
        sleep_ms: u64,
    ) -> Result<FileLock, String> {
        let max_tries = max_tries.max(1);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let handle = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        for try_no in 1..=max_tries {
            let locked = unsafe { libc::flock(handle.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if locked == 0 {
                return Ok(FileLock { handle });
            }
            if try_no == max_tries {
                return Err(format!(
                    "cannot flock {path} after {try_no} tries (locked elsewhere; \
                     the holder may be stalled): {locked}",
                    path = path.display(),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        }
        unreachable!("the loop returns on every arm")
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.handle.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl FileLock {
    /// Whether this handle is still the file `path` names now, by (dev, ino).
    fn matches(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let (Ok(held), Ok(current)) = (self.handle.metadata(), std::fs::metadata(path)) else {
            return false;
        };
        held.dev() == current.dev() && held.ino() == current.ino()
    }
}

/// The ledger's flock and the LEDGER path it guards: the lock lives at the
/// `.lock` sidecar and the caller reads and writes the returned ledger
/// path. An override names an injected test ledger verbatim; None means the
/// real ledger, resolved as late as the lock itself. A lock that cannot be
/// taken degrades to a lock-free read at the same path (the module's
/// fail-open rule), so the caller always holds one path for the lock, the
/// read and the write.
fn ledger_lock(ledger: Option<&Path>) -> (Option<FileLock>, PathBuf) {
    let Some(path) = ledger else {
        return ledger_lock_resolving(ledger_path);
    };
    let lock = lock_path(path);
    match FileLock::try_acquire(&lock) {
        Ok(l) => (Some(l), path.to_path_buf()),
        Err(_) => (None, path.to_path_buf()),
    }
}

/// The real ledger's taker-side guard. The daemon's whole-dir fold renames
/// the machine locks folder in one step, so an open that straddled the
/// rename holds a path whose directory has moved: a stray inode a recreated
/// folder offered. The verify after the flock re-resolves, and a mismatch
/// drops the lock and retakes it on the fresh answer, where every later
/// taker lands.
fn ledger_lock_resolving(resolve: impl Fn() -> PathBuf) -> (Option<FileLock>, PathBuf) {
    let ledger = resolve();
    let fresh = lock_path(&ledger);
    match FileLock::try_acquire(&fresh) {
        Ok(lock) if lock.matches(&lock_path(&resolve())) => return (Some(lock), ledger),
        Ok(lock) => drop(lock),
        Err(_) => {}
    }
    let ledger = resolve();
    let fresh = lock_path(&ledger);
    match FileLock::try_acquire(&fresh) {
        Ok(lock) => (Some(lock), ledger),
        Err(_) => (None, ledger),
    }
}

fn read_ledger(path: &Path) -> (Ledger, LedgerState) {
    if !path.exists() {
        return (Ledger::default(), LedgerState::Absent);
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (Ledger::default(), LedgerState::Unreadable),
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (Ledger::default(), LedgerState::Unreadable);
    };
    let stamps = v
        .get("stamps")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let pair = s.as_array()?;
                    Some((pair.first()?.as_i64()?, pair.get(1)?.as_u64()? as u32))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // Older ledgers carry no caller_stamps; absent reads empty.
    let caller_stamps = v
        .get("caller_stamps")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let triple = s.as_array()?;
                    Some((
                        triple.first()?.as_i64()?,
                        triple.get(1)?.as_str()?.to_string(),
                        triple.get(2)?.as_u64()? as u32,
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let num = |key: &str| v.get(key).and_then(Value::as_i64).unwrap_or(0);
    // Absent core fields read as unknown (-1/0/0): an older ledger written
    // by a deployed fno-agents without this change admits as before.
    let core_remaining = v
        .get("core_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    (
        Ledger {
            stamps,
            caller_stamps,
            backoff_until_ms: num("backoff_until_ms"),
            backoff_step: num("backoff_step"),
            refusal_kind: v
                .get("refusal_kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            last_refusal_ms: num("last_refusal_ms"),
            last_half_open_ms: num("last_half_open_ms"),
            core_remaining,
            core_reset_ms: num("core_reset_ms"),
            core_seen_ms: num("core_seen_ms"),
        },
        LedgerState::Ok,
    )
}

fn write_ledger(path: &Path, ledger: &Ledger, now: i64) -> Result<(), String> {
    let mut out = ledger.clone();
    out.stamps.retain(|(t, _)| now - *t < WINDOW_MS);
    out.caller_stamps.retain(|(t, _, _)| now - *t < WINDOW_MS);
    let body = json!({
        "stamps": out.stamps,
        "caller_stamps": out.caller_stamps,
        "backoff_until_ms": out.backoff_until_ms,
        "backoff_step": out.backoff_step,
        "refusal_kind": out.refusal_kind,
        "last_refusal_ms": out.last_refusal_ms,
        "last_half_open_ms": out.last_half_open_ms,
        "core_remaining": out.core_remaining,
        "core_reset_ms": out.core_reset_ms,
        "core_seen_ms": out.core_seen_ms,
    });
    let dir = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", path.display()))?;
    let tmp = dir.join(format!(".github-request-budget.tmp-{}", std::process::id()));
    std::fs::write(&tmp, body.to_string())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

fn refusal_line(verdict: &Verdict, core_left: i64) -> String {
    // "rate limit" keeps both existing classifiers (_rest.py and
    // stderr_smells_rate_limit) reading a local refusal as secondary, so
    // every current caller backs off without new handling. "HTTP 403" must
    // stay out: it is the marker _quota.record_refusal keys on, and a local
    // conditional refusal must never be recorded as GitHub's.
    if matches!(verdict.cause, Some("core") | Some("core-reserve")) {
        return format!(
            "gh budget: GitHub core rate limit held locally ({cause}: {core_left} left, \
             resets in {retry}s); this command did not reach GitHub. Retry after {retry}s. \
             Ledger: fno-agents fleet-incident gh-budget status",
            cause = verdict.cause.unwrap_or("core"),
            retry = verdict.retry_after_s,
        );
    }
    // The only non-core hold left is the backoff: this token's own real
    // refusal. "HTTP 403" must stay out: it is the marker the refusal
    // recorder keys on, and a local conditional refusal must never be
    // recorded as GitHub's.
    let cause = verdict.cause.unwrap_or("backoff");
    format!(
        "gh budget: GitHub rate limit held locally ({cause}: \
         backoff {}s left); this command did not reach GitHub{}. Retry after {}s. \
         Ledger: fno-agents fleet-incident gh-budget status",
        verdict.retry_after_s,
        if cause == "backoff" {
            " and no other channel serves it (App token: none for this call)"
        } else {
            ""
        },
        verdict.retry_after_s
    )
}

/// May this request go out now? Takes the ledger path, the clock and the cap
/// so tests pass a temp path, a fixed clock, and a small cap.
pub fn admit(path: &Path, argv: &[String], now: i64, cap: u32) -> Verdict {
    admit_with(Some(path), argv, now, cap, &|| false)
}

/// `admit` for a call that may ride another channel: tests pass a fixed
/// answer, production asks the App injector or the built command.
pub fn admit_routed(
    path: &Path,
    argv: &[String],
    now: i64,
    cap: u32,
    other_channel: &dyn Fn() -> bool,
) -> Verdict {
    admit_with(Some(path), argv, now, cap, other_channel)
}

/// The admit body over an optional ledger override: None means the real
/// ledger, and every read, write and journal below uses the path the lock
/// returned, so a fold that landed mid-call reads and writes the file the
/// lock actually holds instead of a straggler copy.
///
/// Every hold here is this machine's own record of a real GitHub refusal on
/// this machine's token. A call on another channel is not held by it:
/// `other_channel` answers whether the call carries a different token (the
/// App installation, or a child `GH_TOKEN`). It is asked only when a hold
/// would refuse, because the App answer can cost a network read.
fn admit_with(
    ledger_override: Option<&Path>,
    argv: &[String],
    now: i64,
    cap: u32,
    other_channel: &dyn Fn() -> bool,
) -> Verdict {
    let points = points_for(argv);
    let (_lock, path) = ledger_lock(ledger_override);
    let (mut ledger, state) = read_ledger(&path);

    // Exempt shapes are admitted and write nothing: a version probe is not
    // traffic, and writing would only churn the file under every gh call.
    if points == 0 {
        return Verdict {
            admitted: true,
            cause: None,
            retry_after_s: 0,
            points_60s: window_points(&ledger, now),
            cap,
            ledger: state,
            refusal: None,
        };
    }

    // The core gate sits before the secondary backoff: an exhausted or
    // reserved core bucket refuses core-shaped requests regardless of the
    // secondary window, while GraphQL reads ride the untouched bucket.
    let (bucket, _) = crate::gh_spend::bucket_for(argv);
    if bucket == crate::gh_spend::Bucket::Core
        && now < ledger.core_reset_ms
        && ledger.core_remaining >= 0
    {
        let reserve = core_reserve() as i64;
        let retry_after_s = ((ledger.core_reset_ms - now) / 1000).max(1);
        let exhausted = ledger.core_remaining == 0;
        // The reserve stands down only the recurring pollers, so the last
        // requests of the bucket land on merges and pushes, never on a
        // watch loop.
        let reserved = reserve > 0
            && ledger.core_remaining <= reserve
            && crate::gh_spend::is_poll_class(&crate::gh_spend::caller_label());
        if (exhausted || reserved) && !other_channel() {
            let mut verdict = Verdict {
                admitted: false,
                cause: Some(if exhausted { "core" } else { "core-reserve" }),
                retry_after_s,
                points_60s: window_points(&ledger, now),
                cap,
                ledger: state,
                refusal: None,
            };
            verdict.refusal = Some(refusal_line(&verdict, ledger.core_remaining));
            crate::gh_spend::journal_verdict(&path, argv, now, &verdict);
            return verdict;
        }
    }

    let backoff_until_ms = ledger.backoff_until_ms;
    // A core-only refusal leaves the GraphQL bucket open, and a call on
    // another token never met this token's refusal: both go out.
    let held = now < backoff_until_ms
        && !(ledger.refusal_kind == "core" && bucket == crate::gh_spend::Bucket::GraphQL)
        && !other_channel();

    if held {
        // One request per 30s as a half-open probe, so recovery at the top
        // gets noticed instead of waited out; the rest are refused with
        // cause `backoff`.
        if ledger.last_half_open_ms == 0 || now - ledger.last_half_open_ms >= HALF_OPEN_MS {
            ledger.last_half_open_ms = now;
            ledger.stamps.push((now, points));
            ledger
                .caller_stamps
                .push((now, crate::gh_spend::caller_label(), points));
            write_ledger(&path, &ledger, now).ok();
            let v = Verdict {
                admitted: true,
                cause: None,
                retry_after_s: 0,
                points_60s: window_points(&ledger, now),
                cap,
                ledger: state,
                refusal: None,
            };
            crate::gh_spend::journal_verdict(&path, argv, now, &v);
            return v;
        }
        let retry_after_s = ((backoff_until_ms - now) / 1000).max(1);
        let mut verdict = Verdict {
            admitted: false,
            cause: Some("backoff"),
            retry_after_s,
            points_60s: window_points(&ledger, now),
            cap,
            ledger: state,
            refusal: None,
        };
        verdict.refusal = Some(refusal_line(&verdict, -1));
        crate::gh_spend::journal_verdict(&path, argv, now, &verdict);
        return verdict;
    }

    // The machine and poller caps refuse nothing (law D3210: no hold on
    // fno's own guess). An over-cap call goes out; the stamps and the
    // journal keep the spend visible to `status` and the spend report.
    let points_60s = window_points(&ledger, now);
    let caller = crate::gh_spend::caller_label();

    ledger.stamps.push((now, points));
    ledger.caller_stamps.push((now, caller.clone(), points));
    write_ledger(&path, &ledger, now).ok();
    let v = Verdict {
        admitted: true,
        cause: None,
        retry_after_s: 0,
        points_60s: points_60s + points as i64,
        cap,
        ledger: state,
        refusal: None,
    };
    crate::gh_spend::journal_verdict(&path, argv, now, &v);
    v
}

/// The admit op's origin judgment. An agent-origin caller (a spawned worker,
/// or an unattended claude thread whose supervisor stripped the identity) is
/// held by the budget like any transport; the user's own typed command is never
/// held, and its verdict is read off the ledger without writing it. An
/// override names an injected test ledger; None reads the real one.
fn admit_for_origin(
    override_path: Option<&Path>,
    argv: &[String],
    now: i64,
    cap: u32,
    agent: bool,
    other_channel: &dyn Fn() -> bool,
) -> Verdict {
    if !agent {
        let path = override_path
            .map(Path::to_path_buf)
            .unwrap_or_else(ledger_path);
        let (ledger, state) = read_ledger(&path);
        return Verdict {
            admitted: true,
            cause: None,
            retry_after_s: 0,
            points_60s: window_points(&ledger, now),
            cap,
            ledger: state,
            refusal: None,
        };
    }
    admit_with(override_path, argv, now, cap, other_channel)
}

/// GitHub traffic is gh itself or a loop-check adapter that wraps it.
pub(crate) fn is_github_program(program: &OsStr) -> bool {
    let Some(name) = std::path::Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    name == "gh" || crate::loopcheck::internal_gh_adapter(name)
}

/// The child stamps each real request for an agent origin, so a transport
/// stamp too charged one call 1+N; this names the spawns whose child
/// consults, and the transport stands down for them. Any unresolved shape
/// reads false, so the transport keeps charging: unknown always means
/// charge.
pub(crate) fn child_charges_at(
    program: &OsStr,
    agent: bool,
    proxy_dir: Option<&Path>,
    path_var: Option<&OsStr>,
) -> bool {
    if !agent {
        return false;
    }
    let Some(name) = std::path::Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    if name == "fno-gh-proxy" || crate::loopcheck::internal_gh_adapter(name) {
        return true;
    }
    if name != "gh" {
        return false;
    }
    let Some(proxy_dir) = proxy_dir else {
        return false;
    };
    let explicit = std::path::Path::new(program)
        .parent()
        .is_some_and(|p| !p.as_os_str().is_empty());
    let resolved = if explicit {
        Some(std::path::PathBuf::from(program))
    } else {
        first_executable_on_path(name, path_var)
    };
    let Some(resolved) = resolved else {
        return false;
    };
    let Some(parent) = resolved.parent() else {
        return false;
    };
    match (parent.canonicalize(), proxy_dir.canonicalize()) {
        (Ok(child), Ok(proxy)) => child == proxy,
        _ => false,
    }
}

/// A bare program name resolves to its first executable hit on `path_var`;
/// no executable hit resolves to None.
fn first_executable_on_path(name: &str, path_var: Option<&OsStr>) -> Option<std::path::PathBuf> {
    let pv = path_var?;
    std::env::split_paths(pv)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.is_file() && m.permissions().mode() & 0o111 != 0
                })
                .unwrap_or(false)
        })
}

/// The env-reading form the spawn doors call: the origin judge, the proxy
/// dir and PATH all come from this process's environment.
pub(crate) fn child_charges(program: &OsStr) -> bool {
    let proxy_dir = std::env::var_os("FNO_GH_PROXY_DIR").map(std::path::PathBuf::from);
    let path_var = std::env::var_os("PATH");
    child_charges_at(
        program,
        crate::spawn_gate_admission::gate_agent_origin(),
        proxy_dir.as_deref(),
        path_var.as_deref(),
    )
}

/// Some(refusal line) when `program args` is GitHub traffic the fleet budget
/// refuses now. None for every other program, every admit, and every
/// disarmed unit-test build (admission_armed).
pub(crate) fn admit_spawn<S: AsRef<OsStr>>(program: &OsStr, args: &[S]) -> Option<String> {
    if !admission_armed() {
        return None;
    }
    // The transport stands down only where the child consults; a user or
    // daemon spawn, and every unresolvable shape, keeps the stamp here.
    if child_charges(program) {
        return None;
    }
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    admit_spawn_real(now_ms(), cap_from_env(), program, &argv, &|| false)
}

/// admit_spawn for a spawn that places the App installation token by
/// default (`AppAuth::Default`) from `cwd`: when a hold would refuse, a
/// read the installation serves goes out on the installation's own budget.
pub(crate) fn admit_spawn_app<S: AsRef<OsStr>>(
    program: &OsStr,
    args: &[S],
    cwd: &Path,
) -> Option<String> {
    if !admission_armed() || child_charges(program) {
        return None;
    }
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    let app = || crate::github_app::token_for_read_argv_in(&argv, cwd).is_some();
    admit_spawn_real(now_ms(), cap_from_env(), program, &argv, &app)
}

/// admit_spawn over the REAL ledger: the lock, the read and the write share
/// one late resolve, so the state-root fold cannot strand them across
/// spellings. Tests inject through admit_spawn_at instead.
pub(crate) fn admit_spawn_real(
    now: i64,
    cap: u32,
    program: &OsStr,
    args: &[String],
    other_channel: &dyn Fn() -> bool,
) -> Option<String> {
    if !is_github_program(program) {
        return None;
    }
    let verdict = admit_with(None, args, now, cap, other_channel);
    if verdict.admitted {
        None
    } else {
        verdict.refusal
    }
}

/// admit_spawn for a caller whose origin is judged first: an agent-origin
/// read is held by the budget, an attended user's never is. The door a
/// Python adapter delegates to uses this in place of the origin-blind
/// admission inside `bounded_read`. Its callers (the gh proxy and the
/// status reads) place the App installation token on a read the
/// installation sees, so in a hold such a read goes out on that token.
pub(crate) fn admit_spawn_for_origin(argv: &[String]) -> Option<String> {
    if !admission_armed() {
        return None;
    }
    if child_charges(std::ffi::OsStr::new("gh")) {
        return None;
    }
    let app = || crate::github_app::token_for_read_argv(argv).is_some();
    let verdict = admit_for_origin(
        None,
        argv,
        now_ms(),
        cap_from_env(),
        crate::spawn_gate_admission::gate_agent_origin(),
        &app,
    );
    if verdict.admitted {
        None
    } else {
        verdict.refusal
    }
}

/// admit_spawn for an already-built Command: the program and args come off
/// the command, so a transport admits without knowing what it was handed.
/// The program check runs before any argv copy: the transports carry every
/// git read too, and those must stay allocation-free.
pub(crate) fn admit_command(cmd: &std::process::Command) -> Option<String> {
    if !admission_armed() || !is_github_program(cmd.get_program()) {
        return None;
    }
    // The same stand-down rule as admit_spawn: only a spawn whose child
    // consults skips this layer; user and daemon reads keep the stamp.
    if child_charges(cmd.get_program()) {
        return None;
    }
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // A child that carries its own GH_TOKEN (the App injector placed it, or
    // the caller chose a token) is a different token than the one refused.
    let own_token = cmd
        .get_envs()
        .any(|(key, value)| key == OsStr::new("GH_TOKEN") && value.is_some());
    admit_spawn_real(now_ms(), cap_from_env(), cmd.get_program(), &args, &|| {
        own_token
    })
}

/// The refusal as a finished child: exit 75 (the Python door's REFUSED),
/// empty stdout, the ledger line on stderr. A caller folds it into its
/// existing "gh failed" path with no new handling.
pub(crate) fn refused_output(line: String) -> std::process::Output {
    std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(75 << 8),
        stdout: Vec::new(),
        stderr: line.into_bytes(),
    }
}

/// Command::output() behind the budget: the door for a direct gh spawn. A
/// refusal never spawns a child; everything else is the command's own output.
pub(crate) fn output(cmd: std::process::Command) -> std::io::Result<std::process::Output> {
    if let Some(line) = admit_command(&cmd) {
        return Ok(refused_output(line));
    }
    crate::pr_status::quota::dispatch_helper(cmd).output()
}

/// Whether the in-process consult is armed. A unit-test build runs dozens of
/// gh-named stubs in one process, and their aggregate stamps would exhaust
/// (or inherit a hold from) the operator's machine-wide ledger - the stubs
/// are not GitHub traffic. The consult disarms there; budget behavior is
/// tested at admit()/record_refusal() with injected paths and clocks, and a
/// test that wants the transport consult live opts in with
/// FNO_GH_BUDGET_TEST_ARMED.
pub(crate) fn admission_armed() -> bool {
    !(cfg!(test) && std::env::var_os("FNO_GH_BUDGET_TEST_ARMED").is_none())
}

/// A GitHub refusal reached this machine's token. One backoff of
/// [`BASE_BACKOFF_MS`] opens in this machine's ledger, and only here: a
/// peer machine holds a different token, so its refusal never holds this
/// one. A no-op while a backoff is already live, so two writers reporting
/// one refusal widen nothing.
pub fn record_refusal(path: &Path, now: i64, kind: &str) -> Result<(), String> {
    record_refusal_body(Some(path), now, kind)
}

/// The real-ledger refusal entry: the lock, the read and the write share
/// one late resolve, so the state-root fold cannot strand them on different
/// spellings. Tests inject through record_refusal instead.
pub(crate) fn record_refusal_real(now: i64, kind: &str) -> Result<(), String> {
    record_refusal_body(None, now, kind)
}

/// The refusal body over an optional ledger override: None means the real
/// ledger, and the write below lands on the path the lock returned.
fn record_refusal_body(ledger_override: Option<&Path>, now: i64, kind: &str) -> Result<(), String> {
    let (_lock, path) = ledger_lock(ledger_override);
    let (mut ledger, _) = read_ledger(&path);
    if now < ledger.backoff_until_ms {
        return Ok(());
    }
    ledger.backoff_step = 0;
    ledger.refusal_kind = kind.to_string();
    ledger.backoff_until_ms = now + BASE_BACKOFF_MS;
    ledger.last_refusal_ms = now;
    write_ledger(&path, &ledger, now)
}

/// The reserve before exhaustion that stands down only the pollers: 0
/// disables the reserve refusals; exhaustion (remaining == 0) refuses
/// regardless of the reserve.
pub(crate) fn core_reserve() -> u32 {
    std::env::var("FNO_GH_CORE_RESERVE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(500)
}

/// The real-ledger core stamp: the lock, the read and the write share one
/// late resolve. Tests inject through their own record_core_at seam.
pub(crate) fn record_core_real(remaining: i64, reset_epoch_s: i64, now: i64) {
    let (_lock, locked) = ledger_lock(None);
    record_core_locked(&locked, remaining, reset_epoch_s, now);
}

/// One core reading lands in the ledger under the flock the caller holds:
/// it writes when the reading is newer than core_seen_ms, and lowers
/// core_remaining only within the same core_reset_ms (a later reset
/// replaces the bucket; an older one is a stale window, dropped). A fresh
/// reading (under 60 s old, same reset, both values above twice the
/// reserve) skips the write, so the write rate stays flat while the
/// bucket is healthy.
fn record_core_locked(path: &Path, remaining: i64, reset_epoch_s: i64, now: i64) {
    let (mut ledger, _) = read_ledger(path);
    let reset_ms = reset_epoch_s * 1000;
    if ledger.core_seen_ms > 0
        && now - ledger.core_seen_ms < 60_000
        && ledger.core_reset_ms == reset_ms
        && ledger.core_remaining > 2 * core_reserve() as i64
        && remaining > 2 * core_reserve() as i64
    {
        return;
    }
    if ledger.core_reset_ms == 0 || reset_ms > ledger.core_reset_ms {
        // A later reset replaces the stored bucket wholesale.
        ledger.core_remaining = remaining;
        ledger.core_reset_ms = reset_ms;
        ledger.core_seen_ms = now;
    } else if reset_ms == ledger.core_reset_ms && now > ledger.core_seen_ms {
        ledger.core_remaining = if ledger.core_remaining < 0 {
            remaining
        } else {
            ledger.core_remaining.min(remaining)
        };
        ledger.core_seen_ms = now;
    } else {
        // An older reset: a stale window's reading, dropped.
        return;
    }
    write_ledger(path, &ledger, now).ok();
}

/// The one core-observation entry the three writers call: it resolves the
/// REAL ledger and no-ops under a unit-test build (a stubbed probe's core-0
/// reading must never land in the operator's live ledger). Opt-in for a
/// test that wants the entry live: FNO_GH_BUDGET_TEST_ARMED.
pub(crate) fn record_core_observation(remaining: i64, reset_epoch_s: i64) {
    if !core_recording_armed() {
        return;
    }
    record_core_real(remaining, reset_epoch_s, now_ms());
}

pub(crate) fn core_recording_armed() -> bool {
    !(cfg!(test) && std::env::var_os("FNO_GH_BUDGET_TEST_ARMED").is_none())
}

/// The core bucket of a `gh api rate_limit` payload, as (remaining, reset
/// epoch s): the account-wide reading the exempt probe already paid for.
pub(crate) fn core_from_rate_limit(payload: &Value) -> Option<(i64, i64)> {
    let core = payload.pointer("/resources/core")?;
    let remaining = core.get("remaining")?.as_i64()?;
    let reset = core.get("reset")?.as_i64()?;
    Some((remaining, reset))
}

/// GitHub's core limit for an unauthenticated request. An authenticated
/// token reads 5000 (or more), so a core limit of exactly 60 means the
/// caller sent no token.
pub(crate) const ANONYMOUS_CORE_LIMIT: i64 = 60;

/// True when a `rate_limit` payload is the anonymous bucket (no token here).
pub(crate) fn is_anonymous_core(payload: &Value) -> bool {
    payload
        .pointer("/resources/core/limit")
        .and_then(Value::as_i64)
        == Some(ANONYMOUS_CORE_LIMIT)
}

/// Whether this machine's `gh` is anonymous, read from the exempt
/// `rate_limit` endpoint. An unreadable answer is not anonymous: it keeps
/// the existing refusal handling.
pub(crate) fn local_gh_anonymous(gh: &str) -> bool {
    // The probe reads the exempt rate_limit endpoint with the resolved gh.
    // It runs under the quota lock, and admission would refuse the very read
    // that diagnoses a held budget, so it spawns directly.
    #[allow(clippy::disallowed_methods)]
    let out = std::process::Command::new(gh)
        .args(["api", "rate_limit"])
        .output();
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice::<Value>(&o.stdout)
            .map(|v| is_anonymous_core(&v))
            .unwrap_or(false),
        _ => false,
    }
}

/// One core reading lands in the ledger from an exempt `rate_limit` answer:
/// the probe the GraphQL broker runs before every discretionary call, the
/// spend report, and the failure classifiers all fetch this payload, and
/// each fetch is a fresh account-wide observation. Without it the ledger's
/// core bucket only moved at conditional-read moments, and a positive
/// reading stamped early in the hour held calls open while the true count
/// sat at 0 (the 2026-10-25 five-hour outage). The same arming and
/// no-op-under-a-unit-test-build rule as [`record_core_observation`].
pub(crate) fn record_core_from_rate_limit(payload: &Value) {
    let Some((remaining, reset)) = core_from_rate_limit(payload) else {
        return;
    };
    // An anonymous reading is this machine's keyless bucket, not the
    // account's: it must not stamp the shared core bucket.
    if is_anonymous_core(payload) {
        return;
    }
    if !core_recording_armed() {
        return;
    }
    record_core_real(remaining, reset, now_ms());
}

/// Read-only projection for status surfaces and the loop gate's pre-read.
pub fn snapshot(path: &Path, now: i64) -> Snapshot {
    let (ledger, state) = read_ledger(path);
    let refusal_kind = ledger.refusal_kind.clone();
    let until = ledger.backoff_until_ms;
    let core_seen_age_s = if ledger.core_seen_ms > 0 {
        ((now - ledger.core_seen_ms).max(0)) / 1000
    } else {
        -1
    };
    Snapshot {
        points_60s: window_points(&ledger, now),
        cap: cap_from_env(),
        backoff_remaining_s: ((until - now).max(0)) / 1000,
        ledger: state,
        refusal_kind,
        core_remaining: ledger.core_remaining,
        core_reset_in_s: if ledger.core_reset_ms > now {
            (ledger.core_reset_ms - now) / 1000
        } else {
            0
        },
        core_seen_age_s,
        // Fresh means the reading still describes THIS bucket: recent AND
        // taken against a reset that is still in the future - the same
        // currency the core gate requires. A reading whose window has
        // rolled over describes a bucket that no longer exists; trusting
        // its positivity near a boundary would downgrade a real core
        // refusal in the new window.
        core_reading_fresh: ledger.core_seen_ms > 0
            && now - ledger.core_seen_ms <= CORE_READING_FRESH_MS
            && now < ledger.core_reset_ms,
        core_reserve: core_reserve(),
    }
}

/// Binary entry under the `fleet-incident` action (law d-fe66560a allows no
/// new client action): `fno-agents fleet-incident gh-budget [status]`, or the
/// stdin door `command_call("fleet-incident", {"op": ...})` takes. One JSON
/// payload on stdin (the `rust_binary.command_call` door convention), one JSON
/// object on stdout. All three ops exit 0; the refusal is carried in the
/// payload, never in the exit code, because `command_call` raises on a nonzero
/// exit. The bare `status` word answers without stdin, so the refusal prose
/// can name a copy-pasteable command.
pub fn run_gh_budget(args: &[String]) -> i32 {
    if args == ["status"] {
        print_status_with_spenders();
        return 0;
    }
    if args == ["spend"] {
        return dispatch_op(&json!({"op": "spend", "hours": 1, "github": true}));
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        eprintln!("gh-budget: stdin read failed");
        return 2;
    }
    let payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gh-budget: bad payload: {e}");
            return 2;
        }
    };
    dispatch_op(&payload)
}

/// The stdin door command_call takes: `fno-agents fleet-incident` with the ops
/// payload on stdin and NO argv. A payload naming a budget op dispatches
/// here; anything else (a bare interactive `fno-agents fleet-incident`, a
/// tty stdin, non-JSON stdin) falls through to the incident breaker's own
/// behavior, which owns its usage error.
pub fn run_gh_budget_stdin_door() -> i32 {
    if std::io::stdin().is_terminal() {
        return -1; // not ours: the caller falls through to the breaker
    }
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return -1;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&buf) else {
        return -1;
    };
    if payload.get("op").and_then(Value::as_str).is_none() {
        return -1;
    }
    dispatch_op(&payload)
}

fn print_status() {
    println!("{}", status_value());
}

/// The bare `status` word a person types after a refusal: the ledger plus
/// the last hour's top core spenders from the journal, so an exhausted
/// bucket names who spent it. The `status` op stays journal-free.
fn print_status_with_spenders() {
    let mut v = status_value();
    v["top_spenders_1h"] =
        crate::gh_spend::top_spenders(&crate::gh_spend::journal_path(), now_ms(), 3);
    v["spend_report"] = json!("fno-agents fleet-incident gh-budget spend");
    println!("{v}");
}

fn status_value() -> Value {
    let s = snapshot(&ledger_path(), now_ms());
    json!({
            "points_60s": s.points_60s,
            "cap": s.cap,
            "backoff_remaining_s": s.backoff_remaining_s,
            "ledger": ledger_word(s.ledger),
            "refusal_kind": s.refusal_kind,
            "core_remaining": s.core_remaining,
            "core_reset_in_s": s.core_reset_in_s,
            "core_seen_age_s": s.core_seen_age_s,
            "core_reading_fresh": s.core_reading_fresh,
            "core_reserve": s.core_reserve,
            "gh_login": crate::gh_login::current(),
    })
}

fn dispatch_op(payload: &Value) -> i32 {
    let now = now_ms();
    match payload.get("op").and_then(Value::as_str) {
        Some("admit") => {
            let argv: Vec<String> = payload
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            // The op is the one place the Python doors' origin is judged;
            // the Rust transports stamp only the spawns whose child does
            // not consult (child_charges).
            let v = admit_for_origin(
                None,
                &argv,
                now,
                cap_from_env(),
                crate::spawn_gate_admission::gate_agent_origin(),
                &|| false,
            );
            println!(
                "{}",
                json!({
                    "verdict": if v.admitted { "admitted" } else { "refused" },
                    "cause": v.cause,
                    "retry_after_s": v.retry_after_s,
                    "points_60s": v.points_60s,
                    "cap": v.cap,
                    "ledger": ledger_word(v.ledger),
                    "refusal": v.refusal,
                })
            );
            0
        }
        Some("refused") => {
            // The caller classifies its own stderr; anything off this list
            // reads unknown rather than failing the record.
            let kind = match payload.get("kind").and_then(Value::as_str) {
                Some(k @ ("secondary" | "core" | "abuse")) => k,
                _ => "unknown",
            };
            match record_refusal_real(now, kind) {
                Ok(()) => {
                    println!("{}", json!({"recorded": true, "kind": kind}));
                    0
                }
                Err(e) => {
                    eprintln!("gh-budget: {e}");
                    1
                }
            }
        }
        Some("status") => {
            print_status();
            0
        }
        Some("spend") => {
            let hours = payload.get("hours").and_then(Value::as_i64).unwrap_or(1);
            let github = if payload
                .get("github")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                // The probe the report is about to print is a fresh
                // account-wide reading; the diagnostic also feeds the
                // ledger, so a status read right after names the truth it
                // just observed.
                let probe = crate::gh_spend::rate_limit_probe();
                if let Ok(reading) = &probe {
                    record_core_from_rate_limit(reading);
                }
                Some(probe)
            } else {
                None
            };
            println!(
                "{}",
                crate::gh_spend::spend_report_at(
                    &crate::gh_spend::journal_path(),
                    now,
                    hours,
                    github,
                )
            );
            0
        }
        other => {
            eprintln!(
                "gh-budget: unknown op {other:?}; expected admit, refused, spend, or status \
                 (fno-agents fleet-incident gh-budget)"
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    /// The injected spawn-admit seam: tests pin a temp ledger, a fixed
    /// clock and a small cap through the shape the production entry takes.
    fn admit_spawn_at(
        path: &Path,
        now: i64,
        cap: u32,
        program: &OsStr,
        args: &[String],
    ) -> Option<String> {
        if !is_github_program(program) {
            return None;
        }
        let verdict = admit(path, args, now, cap);
        if verdict.admitted {
            None
        } else {
            verdict.refusal
        }
    }

    /// The injected core-stamp seam.
    fn record_core_at(path: &Path, remaining: i64, reset_epoch_s: i64, now: i64) {
        let (_lock, locked) = ledger_lock(Some(path));
        record_core_locked(&locked, remaining, reset_epoch_s, now);
    }

    /// A held flock must name itself fast instead of queueing the next shim
    /// call behind a blocking LOCK_EX. The bound is driven through the
    /// parametric form so the test waits 2ms, not the real 2s; the released
    /// lock must free for the next caller.
    #[test]
    fn a_held_flock_names_itself_instead_of_queueing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("github-request-budget.json.lock");
        let held = FileLock::try_acquire(&path).expect("the first caller takes the lock");
        let started = std::time::Instant::now();
        let err = FileLock::try_acquire_with(&path, 2, 1)
            .expect_err("a held lock refuses the second caller");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the bound is short, not a blocking wait: {:?}",
            started.elapsed()
        );
        assert!(
            err.contains("locked elsewhere"),
            "the error names the wait: {err}"
        );
        drop(held);
        assert!(
            FileLock::try_acquire(&path).is_ok(),
            "the lock frees for the next caller"
        );
    }

    #[test]
    fn backoff_holds_this_token_with_one_half_open_probe() {
        // AC2-EDGE: a refusal at t=0 opens a 60s window; a second
        // refusal inside it widens nothing; the +20s admit is the half-open
        // probe; the +35s admit is refused `backoff`; a 0-point version probe
        // at +40s admits and writes nothing.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let t0 = 1_760_000_000_000i64;
        record_refusal(&path, t0, "secondary").unwrap();
        // A second caller (a different session, same machine ledger) reporting
        // the same refusal widens nothing: the budget is per machine token,
        // never session-scoped.
        record_refusal(&path, t0 + 10_000, "secondary").unwrap();
        let (ledger, _) = read_ledger(&path);
        assert_eq!(ledger.backoff_until_ms, t0 + 60_000);

        let v = admit(&path, &argv(&["api", "user"]), t0 + 20_000, 450);
        assert!(v.admitted, "the half-open probe passes");
        let v = admit(&path, &argv(&["api", "user"]), t0 + 35_000, 450);
        assert!(!v.admitted);
        assert_eq!(v.cause, Some("backoff"));

        let before = std::fs::read(&path).unwrap();
        let v = admit(&path, &argv(&["--version"]), t0 + 40_000, 450);
        assert!(v.admitted);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "0-point admit writes nothing"
        );
    }

    #[test]
    fn a_live_hold_routes_other_channels_and_refuses_the_same_token() {
        // Law: a hold is this token's own refusal, and a call on another
        // channel goes out. A ledger pinned at step 4 by an older binary
        // opens only the base window; a peer's hold is never read.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let t0 = 1_760_000_000_000i64;
        std::fs::write(
            &path,
            r#"{"stamps":[],"backoff_until_ms":1760000900000,"backoff_step":4,"refusal_kind":"secondary","last_refusal_ms":1760000000000,"last_half_open_ms":0}"#,
        )
        .unwrap();
        // The pinned hold expired at its own deadline: admitted.
        let v = admit(
            &path,
            &argv(&["api", "repos/o/r/pulls/1"]),
            t0 + 900_000,
            450,
        );
        assert!(v.admitted, "an expired own hold admits");

        record_refusal(&path, t0 + 900_001, "core").unwrap();
        let (ledger, _) = read_ledger(&path);
        assert_eq!(ledger.backoff_until_ms, t0 + 900_001 + 60_000);
        assert_eq!(snapshot(&path, t0 + 900_001).refusal_kind, "core");
        let t1 = t0 + 900_002;
        // The first call in the window takes the half-open probe slot.
        assert!(admit(&path, &argv(&["api", "user"]), t1, 450).admitted);

        let rest = argv(&["api", "repos/o/r/pulls/1"]);
        let v = admit(&path, &rest, t1 + 1_000, 450);
        assert_eq!(v.cause, Some("backoff"), "the same token stays held");
        let line = v.refusal.unwrap();
        assert!(
            line.contains("held locally") && line.contains("no other channel"),
            "{line}"
        );

        let v = admit_routed(&path, &rest, t1 + 2_000, 450, &|| true);
        assert!(v.admitted, "a call on another token goes out");
        let gql = argv(&["api", "graphql", "-f", "query=query{viewer{login}}"]);
        assert!(
            admit(&path, &gql, t1 + 3_000, 450).admitted,
            "a core-only hold leaves GraphQL open"
        );

        record_refusal(&path, t1 + 120_000, "secondary").unwrap();
        assert!(
            admit(&path, &rest, t1 + 120_500, 450).admitted,
            "probe slot"
        );
        let v = admit(&path, &gql, t1 + 121_000, 450);
        assert_eq!(v.cause, Some("backoff"), "a secondary hold covers GraphQL");
    }

    #[test]
    fn refusal_kind_defaults_to_unknown_for_older_ledgers() {
        // AC2-EDGE: a ledger written before the kind field existed reads
        // unknown, and a refusal recorded with no kind stamps unknown.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        std::fs::write(
            &path,
            r#"{"stamps":[],"backoff_until_ms":0,"backoff_step":0,"last_refusal_ms":0,"last_half_open_ms":0}"#,
        )
        .unwrap();
        record_refusal(&path, 1_760_000_000_000, "unknown").unwrap();
        let (ledger, _) = read_ledger(&path);
        assert_eq!(ledger.refusal_kind, "unknown");
        assert_eq!(ledger.backoff_step, 0, "a first refusal opens at the base");
        assert_eq!(ledger.backoff_until_ms, 1_760_000_000_000 + 60_000);
    }

    #[test]
    fn admit_spawn_refuses_github_programs_only() {
        // A live backoff with the half-open probe spent (the shape the
        // 2026-10-15 fleet repro seeded): every 1-point gh ask refuses,
        // other programs never consult the ledger, and a 0-point gh shape
        // admits without a write.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let now = 1_760_000_000_000i64;
        std::fs::write(
            &path,
            r#"{"stamps":[],"backoff_until_ms":1760000600000,"backoff_step":0,"last_refusal_ms":1759999540000,"last_half_open_ms":1760000000000}"#,
        )
        .unwrap();
        let gh = std::ffi::OsStr::new("gh");
        let args = &["api".to_string(), "user".to_string()];
        let line = admit_spawn_at(&path, now, 450, gh, args).expect("gh refuses under the backoff");
        assert!(line.contains("gh budget:"), "line: {line}");

        // An absolute path and the internal coverage adapter are the same
        // traffic under other names.
        let abs = std::ffi::OsStr::new("/opt/homebrew/bin/gh");
        assert!(
            admit_spawn_at(&path, now, 450, abs, args).is_some(),
            "an absolute gh path refuses too"
        );
        let adapter = std::ffi::OsStr::new("fno-gh-coverage");
        let pr_view = &["pr".to_string(), "view".to_string()];
        assert!(
            admit_spawn_at(&path, now, 450, adapter, pr_view).is_some(),
            "the coverage adapter refuses too"
        );

        // git never reaches the ledger; `gh --version` is a 0-point shape
        // that admits untouched. Neither writes a stamp.
        let before = std::fs::read(&path).unwrap();
        let git = std::ffi::OsStr::new("git");
        assert!(admit_spawn_at(&path, now, 450, git, &["status".to_string()]).is_none());
        assert!(admit_spawn_at(&path, now, 450, gh, &["--version".to_string()]).is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before, "no stamp for either");
    }

    #[test]
    fn child_charges_names_consulting_children_for_agents_only() {
        // The stand-down table: an adapter name is a consulting child for an
        // agent and never for anyone else; a bare gh stands down only when
        // its first PATH hit resolves inside the proxy dir; every
        // unresolvable or outside shape charges.
        let tmp = tempfile::tempdir().unwrap();
        let proxy_dir = tmp.path().join("proxy");
        let other_dir = tmp.path().join("other");
        std::fs::create_dir(&proxy_dir).unwrap();
        std::fs::create_dir(&other_dir).unwrap();
        crate::write_exec_stub(&proxy_dir, "gh", "#!/bin/sh\nexit 0\n");
        crate::write_exec_stub(&other_dir, "gh", "#!/bin/sh\nexit 0\n");
        crate::write_exec_stub(&proxy_dir, "fno-gh-proxy", "#!/bin/sh\nexit 0\n");
        let proxy_path: std::ffi::OsString = proxy_dir.clone().into_os_string();
        let other_path: std::ffi::OsString = other_dir.clone().into_os_string();
        let adapter = std::ffi::OsStr::new("fno-gh-coverage");
        let shim = std::ffi::OsStr::new("fno-gh-proxy");
        let gh = std::ffi::OsStr::new("gh");
        let abs = std::ffi::OsStr::new("/opt/homebrew/bin/gh");

        // An adapter stands down for an agent and charges for everyone else.
        assert!(child_charges_at(
            adapter,
            true,
            Some(&proxy_dir),
            Some(&proxy_path)
        ));
        assert!(!child_charges_at(
            adapter,
            false,
            Some(&proxy_dir),
            Some(&proxy_path)
        ));
        // The proxy shim itself consults through the Python doors.
        assert!(child_charges_at(
            shim,
            true,
            Some(&proxy_dir),
            Some(&proxy_path)
        ));
        // A bare gh whose first PATH hit is the proxy shim is a consulting
        // child; the same bare gh found elsewhere is unknown traffic.
        assert!(child_charges_at(
            gh,
            true,
            Some(&proxy_dir),
            Some(&proxy_path)
        ));
        assert!(!child_charges_at(
            gh,
            true,
            Some(&proxy_dir),
            Some(&other_path)
        ));
        // No proxy dir, no resolution, and any absolute gh outside the
        // proxy dir all charge: unknown always means charge.
        assert!(!child_charges_at(gh, true, None, Some(&proxy_path)));
        assert!(!child_charges_at(
            abs,
            true,
            Some(&proxy_dir),
            Some(&proxy_path)
        ));
        assert!(!child_charges_at(gh, true, Some(&proxy_dir), None));
    }

    /// Sets one env var for a door test and restores the previous value on
    /// drop, so a failing assert cannot poison the process env for sibling
    /// tests. Caller-kind tests must hold the shared env lock first.
    struct DoorEnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl DoorEnvGuard {
        fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
            let guard = DoorEnvGuard(vec![(key, std::env::var_os(key))]);
            std::env::set_var(key, value);
            guard
        }
    }
    impl Drop for DoorEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// Seeds a hermetic ledger (FNO_STATE_DIR) with a live backoff whose
    /// half-open probe is already spent, so any consult refuses: Some from
    /// a door proves the consult happened, None under the same ledger
    /// proves the stand-down, and a foreign consult from a sibling test
    /// thread cannot flip either observable.
    fn seed_backoff_ledger(state: &std::path::Path) -> std::path::PathBuf {
        let locks = state.join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        let path = locks.join("github-request-budget.json");
        let now_ms = now_ms();
        std::fs::write(
            &path,
            format!(
                r#"{{"stamps":[],"backoff_until_ms":{},"backoff_step":0,"last_refusal_ms":{},"last_half_open_ms":{}}}"#,
                now_ms + 120_000,
                now_ms - 1_000,
                now_ms
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn admit_spawn_charges_by_caller_kind() {
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let proxy = tmp.path().join("proxy");
        let other = tmp.path().join("other");
        std::fs::create_dir(&proxy).unwrap();
        std::fs::create_dir(&other).unwrap();
        crate::write_exec_stub(&proxy, "gh", "#!/bin/sh\nexit 0\n");
        crate::write_exec_stub(&other, "gh", "#!/bin/sh\nexit 0\n");
        let _state = DoorEnvGuard::set("FNO_STATE_DIR", tmp.path().join("state").as_os_str());
        let _armed = DoorEnvGuard::set("FNO_GH_BUDGET_TEST_ARMED", std::ffi::OsStr::new("1"));
        let _proxy = DoorEnvGuard::set("FNO_GH_PROXY_DIR", proxy.as_os_str());
        seed_backoff_ledger(&tmp.path().join("state"));
        let args = &["api".to_string(), "user".to_string()];
        let gh = std::ffi::OsStr::new("gh");

        // User origin: the transport charges regardless of where gh sits.
        let _attended =
            DoorEnvGuard::set("CLAUDE_CODE_SESSION_ATTENDED", std::ffi::OsStr::new("1"));
        let _path = DoorEnvGuard::set("PATH", proxy.as_os_str());
        assert!(
            admit_spawn(gh, args).is_some(),
            "a user-origin spawn keeps the transport consult"
        );
        drop(_path);

        // Agent origin, bare gh first hit inside the proxy: the child
        // consults, so the transport stands down under a live backoff.
        let _agent = crate::spawn_gate_admission::AgentSelfFixture::set();
        let _path = DoorEnvGuard::set("PATH", proxy.as_os_str());
        assert!(
            admit_spawn(gh, args).is_none(),
            "a consulting child stands the transport down"
        );
        // An adapter name is a consulting child at any PATH.
        let adapter = std::ffi::OsStr::new("fno-gh-coverage");
        assert!(
            admit_spawn(adapter, args).is_none(),
            "an adapter spawn stands the transport down"
        );
        drop(_path);

        // Agent origin, gh resolving outside the proxy: unknown means the
        // transport charges, and the seeded backoff refuses it.
        let _path = DoorEnvGuard::set("PATH", other.as_os_str());
        assert!(
            admit_spawn(gh, args).is_some(),
            "an unresolvable-as-child spawn keeps the consult"
        );
    }

    #[test]
    fn auth_status_reads_zero_points_and_other_auth_subcommands_charge() {
        let argv = |words: &[&str]| words.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            points_for(&argv(&["auth", "status", "--json", "hosts"])),
            0,
            "the login read is exempt so it cannot re-enter admit"
        );
        assert_eq!(points_for(&argv(&["auth", "refresh"])), 1);
    }

    #[test]
    fn core_gate_refuses_at_zero_stands_down_pollers_and_never_outlives_its_reset() {
        // AC3-HP: an exhausted bucket refuses the core read with the
        // reset-shaped retry, never the GraphQL read.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let now = 1_760_000_000_000i64;
        record_core_at(&path, 0, now / 1000 + 600, now);
        let core_read = argv(&["api", "repos/o/r/pulls/1"]);
        let v = admit(&path, &core_read, now, 450);
        assert!(!v.admitted);
        assert_eq!(v.cause, Some("core"));
        assert!((v.retry_after_s - 600).abs() <= 1);
        let graphql = argv(&["api", "graphql", "-f", "query=query { x }"]);
        let v = admit(&path, &graphql, now, 450);
        assert!(v.admitted, "GraphQL is a different bucket: {v:?}");

        // AC3-HP, the reserve: remaining 300 against the default 500
        // reserve refuses the poller and admits the merge. FNO_GH_CALLER
        // pins the label per call, so the test serializes on the env lock
        // and sets it around each admit.
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let path2 = tmp.path().join("budget2.json");
        record_core_at(&path2, 300, now / 1000 + 600, now);
        let wait_read = argv(&["api", "repos/o/r/pulls/7/reviews?per_page=100"]);
        std::env::set_var("FNO_GH_CALLER", "fno do pr wait");
        let v = admit(&path2, &wait_read, now, 450);
        assert!(!v.admitted);
        assert_eq!(v.cause, Some("core-reserve"));
        std::env::set_var("FNO_GH_CALLER", "fno do pr merge");
        let v = admit(&path2, &wait_read, now, 450);
        assert!(v.admitted, "the merge keeps the bucket's tail: {v:?}");
        std::env::remove_var("FNO_GH_CALLER");
        drop(_env);

        // AC3-EDGE: an exhausted reading whose reset has passed gates
        // nothing - an old exhaustion never outlives its reset.
        let path3 = tmp.path().join("budget3.json");
        record_core_at(&path3, 0, now / 1000 - 100, now);
        let v = admit(&path3, &core_read, now, 450);
        assert!(v.admitted, "a passed reset ignores core state: {v:?}");
    }

    #[test]
    fn a_poll_class_caller_past_its_reference_still_admits_and_keeps_counting() {
        // The 2026-11-03 runaway (one unlabeled poller, 12,410 rows at
        // 4.5 Hz for 4.3 h) is why the per-caller spend is counted. Law
        // D3210 is why it refuses nothing: a poll-class caller past
        // POLLER_CAP_PER_MIN admits, and the caller's stamps keep the
        // spend visible to `status`.
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let now = 1_760_000_000_000i64;
        let read = argv(&["api", "repos/o/r/pulls/1"]);
        std::env::set_var("FNO_GH_CALLER", "unknown");
        for _ in 0..POLLER_CAP_PER_MIN {
            let v = admit(&path, &read, now, 450);
            assert!(
                v.admitted && v.cause.is_none(),
                "a poll caller past the reference line still admits: {v:?}"
            );
        }
        let (ledger, _) = read_ledger(&path);
        let caller_60s: i64 = ledger
            .caller_stamps
            .iter()
            .filter(|(t, l, _)| *l == "unknown" && now - *t < WINDOW_MS)
            .map(|(_, _, p)| *p as i64)
            .sum();
        assert_eq!(
            caller_60s, POLLER_CAP_PER_MIN as i64,
            "the caller's own telemetry counts every read"
        );

        // The window ages: the stamps prune, the spend reads fresh.
        let v = admit(&path, &read, now + WINDOW_MS as i64 + 1_000, 450);
        assert!(v.admitted, "a caller's window prunes: {v:?}");
        std::env::remove_var("FNO_GH_CALLER");
        drop(_env);
    }

    #[test]
    fn a_machine_cap_past_its_reference_admits_and_the_window_counts_the_spend() {
        // The law's own specimen: fno refused 176 reads at 25 of 450
        // points while GitHub showed 4886 of 5000 left. Over-cap calls
        // go out; the window telemetry reads the true spend, never a
        // refusal.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let now = 1_760_000_000_000i64;
        let read = argv(&["api", "repos/o/r/pulls/1"]);
        let mut last = None;
        for _ in 0..6 {
            last = Some(admit(&path, &read, now, 5));
        }
        let v = last.unwrap();
        assert!(v.admitted, "a call past the cap admits: {v:?}");
        assert_eq!(v.cause, None);
        assert_eq!(
            v.points_60s, 6,
            "the window telemetry counts every point spent"
        );
        assert_eq!(read_ledger(&path).0.stamps.len(), 6, "each admit stamps");
    }

    #[test]
    fn a_rate_limit_payload_stamps_the_core_bucket_and_engages_the_gate() {
        // The exempt probe's payload names both buckets; the core half is a
        // fresh account-wide reading. Parse: remaining and reset come off
        // /resources/core, and a payload without the bucket reads None.
        let reset_s = now_ms() / 1000 + 600;
        let payload = json!({
            "resources": {
                "core": {"limit": 5000, "used": 5000, "remaining": 0, "reset": reset_s},
                "graphql": {"remaining": 4900, "reset": reset_s},
            }
        });
        assert_eq!(core_from_rate_limit(&payload), Some((0, reset_s)));
        assert_eq!(core_from_rate_limit(&json!({"resources": {}})), None);

        // Limit 60 is GitHub's keyless bucket; a token reads 5000.
        let anonymous =
            json!({"resources": {"core": {"limit": 60, "remaining": 59, "reset": reset_s}}});
        assert!(is_anonymous_core(&anonymous), "limit 60 means no token");
        assert!(!is_anonymous_core(&payload), "limit 5000 is authed");
        assert!(!is_anonymous_core(&json!({"resources": {}})));

        // The full entry under the real ledger: disarmed (the unit-test
        // default) it writes nothing anywhere; armed over a staged state
        // dir it stamps the ledger, and the gate holds the next core read.
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _state = DoorEnvGuard::set("FNO_STATE_DIR", tmp.path().as_os_str());
        let ledger = ledger_path();
        record_core_from_rate_limit(&payload);
        assert!(!ledger.exists(), "a disarmed stamp writes nothing");

        let _armed = DoorEnvGuard::set("FNO_GH_BUDGET_TEST_ARMED", std::ffi::OsStr::new("1"));
        record_core_from_rate_limit(&payload);
        let (stamped, _) = read_ledger(&ledger);
        assert_eq!(stamped.core_remaining, 0);
        assert_eq!(stamped.core_reset_ms, reset_s * 1000);
        let v = admit_with(
            None,
            &argv(&["api", "repos/o/r/pulls/1"]),
            stamped.core_seen_ms,
            450,
            &|| false,
        );
        assert!(!v.admitted, "the fresh exhaustion holds the next core read");
        assert_eq!(v.cause, Some("core"));
    }

    #[test]
    fn the_snapshot_reports_the_core_reading_age_and_freshness() {
        // AC: a positive reading past the freshness bound stops vouching
        // for the bucket, and a never-seen ledger reads unknown, never
        // fresh. This is the field the refusal downgrade consults.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("budget.json");
        let now = 1_760_000_000_000i64;
        record_core_at(&path, 4706, now / 1000 + 3_000, now);
        let s = snapshot(&path, now + 1_000);
        assert!(s.core_reading_fresh, "a just-stamped reading is fresh");
        assert_eq!(s.core_seen_age_s, 1);
        let s = snapshot(&path, now + CORE_READING_FRESH_MS + 2_000);
        assert!(!s.core_reading_fresh, "an old positive vouches for nothing");
        assert_eq!(s.core_seen_age_s, (CORE_READING_FRESH_MS + 2_000) / 1000);
        let empty = tmp.path().join("empty.json");
        let s = snapshot(&empty, now);
        assert_eq!(s.core_seen_age_s, -1);
        assert!(!s.core_reading_fresh);
        // A recent reading against a window that has already rolled over
        // describes a bucket that no longer exists: never fresh, no matter
        // how young it is (the boundary-downgrade finding).
        let rolled = tmp.path().join("rolled.json");
        record_core_at(&rolled, 4706, now / 1000 - 100, now);
        let s = snapshot(&rolled, now + 1_000);
        assert_eq!(s.core_seen_age_s, 1);
        assert!(
            !s.core_reading_fresh,
            "a past-reset reading vouches for nothing"
        );
    }

    // --- the guard against a new bare gh spawn ------------------------------

    /// One production line carrying a bare gh constructor:
    /// `Command::new("gh")`, `Command::new(&gh)`, `Command::new(gh)`, or
    /// `Command::new("fno-gh-<lowercase>")`. Hand-rolled over the line text
    /// so the gate needs no regex dependency.
    fn bare_gh_constructor(line: &str) -> bool {
        let Some(pos) = line.find("Command::new(") else {
            return false;
        };
        let rest = line[pos + "Command::new(".len()..].trim_start();
        let rest = match rest.strip_prefix('&') {
            Some(r) => r.trim_start(),
            None => rest,
        };
        let after = if let Some(quoted) = rest.strip_prefix('"') {
            let Some(end) = quoted.find('"') else {
                return false;
            };
            let name = &quoted[..end];
            let named = name == "gh"
                || (name.starts_with("fno-gh-")
                    && name["fno-gh-".len()..]
                        .chars()
                        .all(|c| c.is_ascii_lowercase()));
            if !named {
                return false;
            }
            &quoted[end + 1..]
        } else {
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if ident != "gh" {
                return false;
            }
            &rest[ident.len()..]
        };
        after.trim_start().starts_with(')')
    }

    /// The cfg(test) items cut out by brace depth, the skipper
    /// scripts/ci/check-mux-process-admission.sh uses. Returns (number,
    /// line) so hits keep their on-disk line numbers.
    fn production_lines(source: &str) -> Vec<(usize, String)> {
        let mut kept = Vec::new();
        let mut pending_test_item = false;
        let mut skip_depth: i64 = 0;
        for (idx, line) in source.lines().enumerate() {
            let stripped = line.trim();
            if skip_depth > 0 {
                skip_depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
                if skip_depth <= 0 {
                    skip_depth = 0;
                }
                continue;
            }
            if pending_test_item {
                if stripped.is_empty() || stripped.starts_with("//") {
                    continue;
                }
                pending_test_item = false;
                let depth = line.matches('{').count() as i64 - line.matches('}').count() as i64;
                if depth > 0 {
                    skip_depth = depth;
                }
                continue;
            }
            if stripped == "#[cfg(test)]" {
                pending_test_item = true;
                continue;
            }
            kept.push((idx + 1, line.to_string()));
        }
        kept
    }

    fn fn_name_of(line: &str) -> Option<String> {
        let pos = line.find("fn ")?;
        let rest = &line[pos + 3..];
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if ident.is_empty() {
            None
        } else {
            Some(ident)
        }
    }

    /// Every production `Command::new(gh...)` whose enclosing fn reaches no
    /// admitting sink (`gh_budget::output(` or `bounded_cmd::output_with_timeout`),
    /// minus the allowlist. Pure over the source so a mutated fixture can
    /// drive it, in the shape `direct_wait_bypasses` established.
    fn bare_gh_spawns(rel_path: &str, source: &str) -> Vec<String> {
        // Test files carry gh-named stubs by design; the walk skips them.
        if rel_path.contains("/tests/")
            || rel_path.ends_with("tests.rs")
            || rel_path.ends_with("_tests.rs")
        {
            return Vec::new();
        }
        let lines = production_lines(source);
        let hit_idxs: Vec<usize> = lines
            .iter()
            .enumerate()
            // A comment never spawns; a doc aside quoting the constructor
            // must not read as a violation.
            .filter(|(_, (_, line))| {
                !line.trim_start().starts_with("//") && bare_gh_constructor(line)
            })
            .map(|(i, _)| i)
            .collect();
        let mut violations = Vec::new();
        for &idx in &hit_idxs {
            // The fn runs from the last `fn ` line at or above the hit to
            // the next `fn ` line below it; the file top counts as one fn.
            // Comment lines never bound a fn: a `// fn foo()` aside must not
            // cut the body short of its admitting sink.
            let is_code = |l: &String| !l.trim_start().starts_with("//");
            let mut start = 0usize;
            let mut fn_name = String::new();
            for i in (0..=idx).rev() {
                if !is_code(&lines[i].1) {
                    continue;
                }
                if let Some(name) = fn_name_of(&lines[i].1) {
                    start = i;
                    fn_name = name;
                    break;
                }
            }
            let mut end = lines.len();
            for i in (idx + 1)..lines.len() {
                if is_code(&lines[i].1) && lines[i].1.contains("fn ") {
                    end = i;
                    break;
                }
            }
            let body = lines[start..end]
                .iter()
                .map(|(_, l)| l.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let behind_sink = body.contains("gh_budget::output(")
                || body.contains("bounded_cmd::output_with_timeout");
            // Allowlist (one site, one reason each):
            // - crates/fno/src/setup_run.rs step_gh_auth: the `fno` crate
            //   cannot depend on fno-agents, and the one-time `gh auth
            //   status` probe carries no fleet identity (a human runs setup
            //   once).
            // - fno-agents/src/gate/merge_hold.rs disarm_command: the emergency
            //   auto-merge disarm must reach GitHub even under a fleet hold;
            //   an armed queue merging server-side is the outcome the hold
            //   exists to prevent.
            // - fno-agents/src/gate/github_app.rs token_for_read_argv_in: the
            //   Command is never spawned; it exists so injection_decision
            //   can read its env, args and cwd for the token decision.
            let allowlist = [
                ("fno/src/setup_run.rs", "step_gh_auth"),
                ("fno-agents/src/gate/merge_hold.rs", "disarm_command"),
                (
                    "fno-agents/src/gate/github_app.rs",
                    "token_for_read_argv_in",
                ),
            ];
            let allowlisted = allowlist
                .iter()
                .any(|(p, f)| rel_path == *p && fn_name == *f);
            if !behind_sink && !allowlisted {
                let (number, line) = &lines[idx];
                violations.push(format!("{rel_path}:{number}: {line}"));
            }
        }
        violations
    }

    fn literal_hits(source: &str) -> Vec<(usize, String)> {
        production_lines(source)
            .into_iter()
            .filter(|(_, line)| !line.trim_start().starts_with("//") && bare_gh_constructor(line))
            .collect()
    }

    fn rs_files_under(
        dir: &std::path::Path,
        rel_base: &std::path::Path,
        out: &mut Vec<(String, String)>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files_under(&path, rel_base, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let rel = path
                    .strip_prefix(rel_base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                if let Ok(source) = std::fs::read_to_string(&path) {
                    out.push((rel, source));
                }
            }
        }
    }

    #[test]
    fn every_gh_spawn_sits_behind_the_budget_seam() {
        // (c) Every transport still carries its admission call in the
        // production half: a removed consult fails here before a spawn can
        // slip past it.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let transports = [
            "src/bounded_cmd.rs",
            "src/gate/gate_probes.rs",
            "src/runtime/org_board/budget.rs",
            "src/gate/loopcheck/bounded_run.rs",
            "src/gate/pr_draft_ready.rs",
            "src/backlog/pr_link.rs",
            "src/runtime/operator_notice.rs",
        ];
        for rel in transports {
            let path = manifest.join(rel);
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let prod_text = production_lines(&source)
                .iter()
                .map(|(_, l)| l.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                prod_text.contains("gh_budget::admit_"),
                "transport {rel} lost its fleet-budget admission"
            );
        }

        let crates_dir = manifest.parent().unwrap().to_path_buf();
        let mut files = Vec::new();
        rs_files_under(&manifest.join("src"), &crates_dir, &mut files);
        rs_files_under(&crates_dir.join("fno/src"), &crates_dir, &mut files);

        let mut raw = 0usize;
        let mut violations = Vec::new();
        for (rel_path, source) in &files {
            raw += literal_hits(source).len();
            violations.extend(bare_gh_spawns(rel_path, source));
        }
        // (b) A wrong haystack must not read clean: the walk on the real
        // tree finds well over a dozen literal gh constructors.
        assert!(
            raw >= 12,
            "the haystack decayed: only {raw} literal gh constructors found"
        );

        // (a) The scanner refuses a bare fn and passes the same fn behind
        // the door.
        let bare = "fn f() {\n    std::process::Command::new(\"gh\").args([\"api\", \"x\"]).output();\n}\n";
        assert_eq!(bare_gh_spawns("x.rs", bare).len(), 1, "bare fn is a hit");
        let behind = "fn f() {\n    let cmd = std::process::Command::new(\"gh\");\n    let _ = crate::gh_budget::output(cmd);\n}\n";
        assert!(
            bare_gh_spawns("x.rs", behind).is_empty(),
            "fn behind the door passes"
        );

        assert!(
            violations.is_empty(),
            "bare gh spawns outside the fleet budget seam:\n{}\n\
             route each through crate::gh_budget::output or a bounded_cmd \
             transport, or add an allowlist entry with a reason",
            violations.join("\n")
        );
    }
}
