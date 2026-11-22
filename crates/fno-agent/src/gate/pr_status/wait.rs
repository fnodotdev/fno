//! The wait loop (`fno do pr wait`): the one sanctioned watcher. Ported
//! from `fno.pr._wait` on top of the status cache: every tick coalesces, a
//! backoff window serves the row row degraded and the loop rides it out, and
//! the exit prints the gh spend, so a caller re-arms on truth.

use crate::pr_status_facts::{GhProbe, RealGhProbe};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub(crate) const MIN_INTERVAL: f64 = 5.0;

/// The pending-backoff ceiling: while checks run, the gap between ticks
/// doubles up to here, then holds. A red or settled verdict is noticed at
/// the next tick, so the cap is the worst-case wake latency; the fleet's
/// own check cadence measures gaps in quarters of an hour, so five minutes
/// buys five times the ticks for a latency the callers already tolerate.
pub(crate) const PENDING_CAP: f64 = 300.0;

fn duration_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)^(\d+(?:\.\d+)?)([smh])?$").unwrap())
}

/// `30m` / `90s` / `1.5h` / plain seconds -> seconds; 0 on garbage.
pub(crate) fn parse_duration(text: &str) -> f64 {
    let Some(c) = duration_re().captures(text.trim()) else {
        return 0.0;
    };
    let n: f64 = c[1].parse().unwrap_or(0.0);
    let mult = match c.get(2).map(|m| m.as_str().to_lowercase()).as_deref() {
        Some("m") => 60.0,
        Some("h") => 3600.0,
        _ => 1.0,
    };
    n * mult
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The local HEAD sha, free: the zero-network tick's head guard. The wait
/// runs from a worktree, so `git rev-parse HEAD` answers without GitHub;
/// None on any failure sends the tick down the network path.
fn local_head_sha(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && sha.len() >= 40).then_some(sha)
}

/// The caller's pin: a literal sha, `HEAD` for the local checkout's own
/// HEAD (read free from git, no GitHub), or nothing. `HEAD` is the recipe
/// spelling: a command substitution cannot ride in the hinted command,
/// because a worktree session's Bash guard refuses one.
#[derive(Debug)]
pub(crate) enum ExpectHead {
    None,
    Sha(String),
    LocalUnresolved,
}

pub(crate) fn resolve_expect_head(payload: &Value) -> ExpectHead {
    let Some(raw) = payload
        .get("expect_head")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return ExpectHead::None;
    };
    if raw == "HEAD" {
        let cwd = Path::new(payload.get("cwd").and_then(Value::as_str).unwrap_or(""));
        return match local_head_sha(cwd) {
            Some(sha) => ExpectHead::Sha(sha),
            None => ExpectHead::LocalUnresolved,
        };
    }
    ExpectHead::Sha(raw.to_string())
}

/// The deterministic resume offset that spreads a fleet's waiters across
/// one interval instead of stacking them at hold expiry. Hash-derived from
/// (slug, pr): one PR keeps one phase across runs; different PRs spread.
/// No randomness, so a rerun resumes at the same place in the window.
pub(crate) fn resume_phase(slug: &str, pr: u64, interval: f64) -> f64 {
    if interval <= 0.0 {
        return 0.0;
    }
    let mut h = 5381u64;
    for b in slug.as_bytes() {
        h = h.wrapping_mul(0x0100_0000_0193).wrapping_add(u64::from(*b));
    }
    h = h
        .wrapping_mul(0x0100_0000_0193)
        .wrapping_add(pr.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (h % 10_000) as f64 / 10_000.0 * interval
}

/// The note that separates this command from the hand-rolled loops it
/// replaces: the spender sees its spend at exit.
fn spend_note(calls: usize, ticks: usize, noun: &str) -> String {
    let snap = crate::gh_budget::snapshot(&crate::gh_budget::ledger_path(), now_ms());
    format!(
        "note: {calls} gh call(s) over {ticks} {noun} read(s) this invocation, \
         fleet budget {} of {} points in the last 60s{}\n",
        snap.points_60s,
        snap.cap,
        format!(
            "{}{}",
            crate::gh_etag::note_suffix(),
            crate::gh_poller::note_suffix()
        )
    )
}

/// One cache-coalesced tick's re-emit: the payload JSON on stdout, the
/// verdict and serve lines on stderr, then the spend note. Only the FINAL
/// tick's output is emitted (`_wait._emit`).
fn emit_tick(payload: &Value, lines: &[String], calls: usize, ticks: usize) -> (String, String) {
    let stdout = format!("{}\n", serde_json::to_string(payload).unwrap_or_default());
    let mut stderr = String::new();
    if !lines.is_empty() {
        stderr.push_str(&lines.join("\n"));
        stderr.push('\n');
    }
    stderr.push_str(&spend_note(calls, ticks, "status"));
    (stdout, stderr)
}

/// The settled/red/green loop. `poll` is one cache-coalesced tick returning
/// (code, payload, stderr lines, this tick's gh calls); `on_red` cancels
/// the red head's runs still in flight and returns one line per run, or
/// None when the live read shows the red belongs to an earlier attempt, so
/// the wait keeps waiting; `sleeper` and `now` are injectable so tests
/// stage the clock. Only the final tick re-emits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn wait_status(
    pr: u64,
    until: &str,
    expect_head: Option<&str>,
    timeout: f64,
    interval: f64,
    mut poll: impl FnMut() -> (i32, Value, Vec<String>, usize),
    mut on_red: impl FnMut(&Value) -> Option<Vec<String>>,
    mut sleeper: impl FnMut(f64),
    now: impl Fn() -> f64,
) -> (i32, String, String) {
    let interval = interval.max(MIN_INTERVAL);
    let deadline = now() + timeout.max(0.0);
    let mut ticks = 0usize;
    let mut calls = 0usize;
    let mut pending_ticks: i32 = 0;
    loop {
        ticks += 1;
        let (rc, mut payload, lines, tick_calls) = poll();
        calls += tick_calls;
        // The payload already names the conflict. A degraded backoff serve
        // (`stale_reason`) cannot prove the head is still the conflicting
        // one, so it rides out; a non-open PR is not a rebase errand.
        let conflicting = payload.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING")
            && payload.get("pr_state").and_then(Value::as_str) == Some("OPEN")
            && payload.get("stale_reason").is_none();
        if conflicting {
            let head = payload
                .get("head")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(8)
                .collect::<String>();
            let (stdout, stderr) = emit_tick(&payload, &lines, calls, ticks);
            let stderr = format!(
                "{stderr}wait: PR {pr} is CONFLICTING at {head}. Integrate the \
                 base (`fno do pr push`), then re-arm the wait.\n"
            );
            return (5, stdout, stderr);
        }
        // The pinned wait: the caller named the sha it pushed, so a live read
        // at any other head would answer settled about the wrong commit (the
        // merged-PR shape: the PR head stops moving while the branch does).
        // Exit 6 at once; waiting cannot fix it. A degraded serve names no
        // head and rides out like a stale red, so a backoff never fakes a
        // mismatch.
        if let Some(expected) = expect_head {
            let degraded = payload.get("stale_reason").is_some();
            let actual = payload.get("head").and_then(Value::as_str).unwrap_or("");
            if !degraded && !actual.is_empty() && !actual.eq_ignore_ascii_case(expected) {
                let note = payload
                    .as_object_mut()
                    .and_then(|obj| super::compose::apply_expect_head(obj, expected));
                if let Some(note) = note {
                    let mut lines = lines;
                    if let Some(first) = lines.first_mut() {
                        *first = super::compose::verdict_line(&payload);
                    }
                    let (stdout, mut stderr) = emit_tick(&payload, &lines, calls, ticks);
                    stderr.push_str(&note);
                    stderr.push('\n');
                    return (6, stdout, stderr);
                }
            }
        }
        let done = match until {
            "green" => payload.get("green").and_then(Value::as_bool),
            _ => payload.get("settled").and_then(Value::as_bool),
        };
        // One failed check already decides this head: cancel its runs still
        // in flight and wake now, so the fix starts and the CI slots free.
        // `green` waits for every check instead. A degraded serve cannot
        // prove the failure belongs to the current head, so it rides out.
        let red = until != "green"
            && payload.get("verdict").and_then(Value::as_str) == Some("red")
            && payload.get("stale_reason").is_none();
        let mut stale_red = false;
        if red {
            match on_red(&payload) {
                Some(cancelled) => {
                    let (stdout, mut stderr) = emit_tick(&payload, &lines, calls, ticks);
                    for line in cancelled {
                        stderr.push_str(&line);
                        stderr.push('\n');
                    }
                    return (rc, stdout, stderr);
                }
                // No live verdict: the red is the old attempt's or made only
                // of cancelled checks, and so is its settled flag. Wait for
                // the rerun, cancel nothing.
                None => stale_red = true,
            }
        }
        if done == Some(true) && !stale_red {
            let (stdout, stderr) = emit_tick(&payload, &lines, calls, ticks);
            return (rc, stdout, stderr);
        }
        // While checks run, double the gap up to the cap: a pending PR buys
        // its live reads on a growing cadence instead of every interval.
        // Any other payload (red, green, conflicting, degraded, settled)
        // restores the base interval, so a wake stays one tick away.
        let pending = payload.get("verdict").and_then(Value::as_str) == Some("pending")
            && payload.get("settled").and_then(Value::as_bool) != Some(true);
        pending_ticks = if pending {
            pending_ticks.saturating_add(1)
        } else {
            0
        };
        let nap = if pending_ticks > 0 {
            (interval * 2f64.powi(pending_ticks - 1)).min(interval.max(PENDING_CAP))
        } else {
            interval
        };
        if now() + nap > deadline {
            let (stdout, stderr) = emit_tick(&payload, &lines, calls, ticks);
            let verdict = payload
                .get("verdict")
                .and_then(Value::as_str)
                .unwrap_or("None");
            let stderr = format!(
                "{stderr}wait: still not {until} after {}s; last verdict {verdict}. \
                 Re-arm the wait or read the PR.\n",
                timeout as i64
            );
            // A red with no live verdict times out pending, never red.
            let code = if rc == 0 || stale_red { 2 } else { rc };
            return (code, stdout, stderr);
        }
        sleeper(nap);
    }
}

/// The review loop: wake when a NEW review posts past the first-read
/// baseline. An unreadable tick is no-answer, never "no new review", so an
/// API blip cannot fake a wake.
pub(crate) fn wait_review(
    pr: u64,
    timeout: f64,
    interval: f64,
    mut counter: impl FnMut() -> Option<i64>,
    mut sleeper: impl FnMut(f64),
    now: impl Fn() -> f64,
    calls: impl Fn() -> usize,
) -> (i32, String, String) {
    let interval = interval.max(MIN_INTERVAL);
    let deadline = now() + timeout.max(0.0);
    let mut baseline: Option<i64> = None;
    let mut last: Option<i64> = None;
    let mut ticks = 0usize;
    loop {
        ticks += 1;
        if let Some(count) = counter() {
            last = Some(count);
            if baseline.is_none() {
                baseline = Some(count);
            } else if count > baseline.unwrap_or(0) {
                let b = baseline.unwrap_or(0);
                let stderr = format!(
                    "wait: {} new review(s) on PR {pr} ({b} -> {count}).\n{}",
                    count - b,
                    spend_note(calls(), ticks, "review")
                );
                return (0, String::new(), stderr);
            }
        }
        if now() + interval > deadline {
            let observed = match last {
                None => "no read succeeded".to_string(),
                Some(n) => format!("last count {n}"),
            };
            let stderr = format!(
                "wait: still no new review after {}s; {observed}. \
                 Re-arm the wait or read the PR.\n",
                timeout as i64,
            );
            return (2, String::new(), stderr);
        }
        sleeper(interval);
    }
}

/// The `status-wait` door op: `{cwd, pr, until, timeout, interval}`.
/// Durations parse from the CLI's raw strings (`15m`, `60`).
pub(crate) fn run_wait(payload: &Value) -> (i32, String, String) {
    let cwd_str = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    // The flag arguments validate first: a usage error answers before the
    // resolve spends a read.
    let field = |key: &str, default: f64| -> f64 {
        match payload.get(key) {
            Some(Value::String(s)) => parse_duration(s),
            Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
            _ => default,
        }
    };
    let timeout = field("timeout", 900.0);
    let interval = field("interval", 60.0);
    if timeout <= 0.0 || interval <= 0.0 {
        return (
            2,
            String::new(),
            "fno do pr wait: --timeout/--interval must parse (30m, 90s, 1h)\n".into(),
        );
    }
    let until = payload
        .get("until")
        .and_then(Value::as_str)
        .unwrap_or("settled");
    if !matches!(until, "settled" | "red" | "green" | "review") {
        return (
            2,
            String::new(),
            "fno do pr wait: --until must be one of settled/red/green/review\n".into(),
        );
    }
    // The bare spelling (pr 0) resolves the current branch's PR once, up
    // front, over the same branch-scoped read bare `pr info` uses; a resolve
    // failure is a usage refusal naming the explicit spelling, never a
    // 404-per-tick loop against PR 0.
    let cwd = Path::new(cwd_str);
    let pr = match super::lists::operand_pr_number(payload, cwd, "fno do pr wait") {
        Ok(pr) => pr,
        Err(reason) => return (2, String::new(), format!("{}\n", reason.text)),
    };
    let sleeper = |s: f64| std::thread::sleep(std::time::Duration::from_secs_f64(s));
    let start = Instant::now();
    let clock = move || start.elapsed().as_secs_f64();
    // The session node's PR repo pins every PR read: the repo the node's
    // rows record for THIS pr, so a cross-repo additional_prs row pins its
    // own repo and a number no row names keeps the ambient origin (the
    // cutover shape: repo A checkout, repo B node).
    let slug_override: Option<String> =
        crate::gate::loopcheck::pr_read::session_node_pr_repo(cwd, pr);
    if until == "review" {
        // The slug resolves once, not per tick; an unresolvable slug retries
        // per tick, so a transient failure never wedges the wait. The node
        // pin outranks the ambient origin on both the first resolve and the
        // per-tick retry.
        let pinned = slug_override
            .clone()
            .or_else(|| super::cache::git_slug(cwd));
        let probe = crate::pr_status::cache::CountingProbe {
            inner: RealGhProbe,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let counter = || {
            // No hold skip: the read chain (app-token swap, GraphQL route)
            // answers through a live hold, so a rate-limited tick still
            // counts reviews instead of riding out to hold expiry.
            let slug = match &pinned {
                Some(s) => s.clone(),
                None => match slug_override
                    .clone()
                    .or_else(|| super::cache::git_slug(cwd))
                {
                    Some(s) => s,
                    None => return None,
                },
            };
            review_count(&probe, cwd, &slug, pr)
        };
        let calls = || probe.calls.load(Ordering::SeqCst);
        return wait_review(pr, timeout, interval, counter, sleeper, clock, calls);
    }
    let expect_head = match resolve_expect_head(payload) {
        ExpectHead::None => None,
        ExpectHead::Sha(sha) => Some(sha),
        ExpectHead::LocalUnresolved => {
            return (
                4,
                String::new(),
                "fno do pr wait: --expect-head HEAD cannot resolve in this checkout; \
                 run it from the worktree that holds the commit\n"
                    .into(),
            )
        }
    };
    // The resume spread keys on the repo and PR: resolved once here, and
    // the hold state carried across ticks so the live->clear edge fires
    // the phase sleep exactly once.
    let slug = super::cache::git_slug(cwd);
    let mut prev_hold_live = crate::gh_budget::snapshot(&crate::gh_budget::ledger_path(), now_ms())
        .backoff_remaining_s
        > 0;
    let mut poll = || {
        // The hold read here only spaces the fleet's first post-hold read
        // (the transition beat below): it no longer gates the tick's reads.
        // The route chain answers through a live hold, so the old skip
        // manufactured "no answer" while the app-token and GraphQL routes
        // were still untried. Retrying on a fixed interval is what
        // sustained the 2026-10-18 secondary-refusal herd, so the spread
        // beat stays.
        let hold_live = crate::gh_budget::snapshot(&crate::gh_budget::ledger_path(), now_ms())
            .backoff_remaining_s
            > 0;
        if prev_hold_live && !hold_live {
            if let Some(s) = &slug {
                // The spread never spends past the caller's deadline: a hold
                // clearing on the last scheduled tick would otherwise sleep
                // a full interval and read after the timeout expired.
                let remaining = (timeout - start.elapsed().as_secs_f64()).max(0.0);
                let phase = resume_phase(&s.replace('/', "--"), pr, interval).min(remaining);
                if phase > 0.0 {
                    std::thread::sleep(std::time::Duration::from_secs_f64(phase));
                }
            }
        }
        prev_hold_live = hold_live;
        // The zero-network tick: a young row minted for the local HEAD
        // answers with no gh call - the head guard is the sha local git
        // read for free. A miss (no row, expired, a pushed head) falls to
        // the network path below, same as today.
        if let Some(sha) = local_head_sha(cwd) {
            if let Some((code, payload, lines)) =
                super::cache::fresh_tick(cwd, pr, Some(&sha), slug_override.as_deref())
            {
                return (code, payload, lines, 0);
            }
        }
        // No hold gate on the head read: the route chain answers through a
        // live hold, so the tick reads the head it would have skipped and
        // the cache serves the row minted for THAT answer. The spread beat
        // above still spaces the fleet's first post-hold read.
        let (head, head_calls) = super::cache::head_read_in(cwd, pr, slug_override.as_deref());
        let block = head.unwrap_or_else(|| json!({"head_sha": ""}));
        let (code, payload, lines, tick_calls) = super::cache::cached_status_in(
            cwd_str,
            pr,
            false,
            Some(&block),
            true,
            slug_override.as_deref(),
        );
        (code, payload, lines, tick_calls + head_calls)
    };
    let on_red = |payload: &Value| {
        let head = payload.get("head").and_then(Value::as_str).unwrap_or("");
        cancel_in_flight("gh", cwd, pr, head)
    };
    wait_status(
        pr,
        until,
        expect_head.as_deref(),
        timeout,
        interval,
        &mut poll,
        on_red,
        sleeper,
        clock,
    )
}

/// Whether the red holds no live verdict. A cancelled check is no result,
/// so a red made only of cancelled checks reads pending here. Otherwise:
/// whether every failure in the live `latest` rows predates its run's
/// current attempt. A rerun keeps the run id and restarts the run clock,
/// so a failed job that completed before `run_started_at` is the old
/// attempt's. Two windows serve that red: a cached status row minted
/// before the rerun, and the check-runs list before the rerun mints its
/// jobs. Live rows with no failure at all are stale too: the red was the
/// cache's. With nothing left in flight an old failure is final, as after
/// a partial rerun. A failed row with no completion time or no listed run
/// is never stale.
fn red_is_stale(latest: &[Value], runs: &[Value]) -> bool {
    let started: std::collections::HashMap<u64, &str> = runs
        .iter()
        .filter_map(|run| {
            Some((
                run.get("id")?.as_u64()?,
                run.get("run_started_at")?.as_str()?,
            ))
        })
        .collect();
    fn bucket(row: &Value) -> &str {
        row.get("bucket").and_then(Value::as_str).unwrap_or("")
    }
    // A cancelled attempt can leave its jobs reading `fail`, though the run
    // itself concluded `cancelled`: a ghost, no result, so it is no failure.
    #[derive(serde::Deserialize)]
    struct RunRef {
        id: Option<u64>,
        conclusion: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct RowLink {
        link: Option<String>,
    }
    let cancelled_runs: std::collections::HashSet<u64> = runs
        .iter()
        .filter_map(|run| serde_json::from_value::<RunRef>(run.clone()).ok())
        .filter(|run| run.conclusion.as_deref() == Some("cancelled"))
        .filter_map(|run| run.id)
        .collect();
    let failed: Vec<&Value> = latest
        .iter()
        .filter(|row| bucket(row) == "fail")
        .filter(|row| {
            let link = serde_json::from_value::<RowLink>((*row).clone())
                .ok()
                .and_then(|r| r.link)
                .unwrap_or_default();
            !crate::pr_push::run_id(&link).is_some_and(|id| cancelled_runs.contains(&id))
        })
        .collect();
    // A queued rerun shows in the runs listing before it mints a check row.
    let in_flight = latest
        .iter()
        .any(|row| !matches!(bucket(row), "pass" | "fail" | "skipping" | "cancel"))
        || runs
            .iter()
            .any(|run| run.get("status").and_then(Value::as_str) != Some("completed"));
    if failed.is_empty() {
        return true;
    }
    in_flight
        && failed.iter().all(|row| {
            let done = row.get("completedAt").and_then(Value::as_str).unwrap_or("");
            let link = row.get("link").and_then(Value::as_str).unwrap_or("");
            match crate::pr_push::run_id(link).and_then(|id| started.get(&id)) {
                Some(attempt_start) => !done.is_empty() && done < *attempt_start,
                None => false,
            }
        })
}

/// Whether every failed row is the ci-required aggregate itself: the row is
/// keyed by its workflow file, not the job display name.
fn hold_only_red(latest: &[Value]) -> bool {
    let failed: Vec<&Value> = latest
        .iter()
        .filter(|row| row.get("bucket").and_then(Value::as_str) == Some("fail"))
        .collect();
    !failed.is_empty()
        && failed.iter().all(|row| {
            row.get("workflow")
                .and_then(Value::as_str)
                .map(|w| w.ends_with("/ci-required.yml"))
                .unwrap_or(false)
        })
}

/// Whether the pull request carries the `pr-hold` label, the same read the
/// ci-required job itself makes. A failed or unreadable read answers false,
/// so the cancel keeps running: an unreadable label state must not disable
/// the fail-fast cancel.
fn pr_hold_label_live(gh_bin: &str, cwd: &Path, pr: u64) -> bool {
    let raw = crate::pr_push::gh_api(
        gh_bin,
        cwd,
        &format!("repos/{{owner}}/{{repo}}/issues/{pr}/labels"),
        &[],
    );
    let raw = match raw {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    parsed
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .any(|l| l.get("name").and_then(Value::as_str) == Some("pr-hold"))
        })
        .unwrap_or(false)
}

/// Cancel the Actions runs still testing a red `head`, one line per run;
/// None when the live rows show the red belongs to an earlier attempt, or
/// when the red is the pr-hold label speaking through the ci-required
/// aggregate with no real failing row beside it - a red no commit on this
/// branch heals, so the wait keeps polling until the hold releases.
/// A failed read or cancel is printed, never hidden. The cancel lands one
/// `ci_wait_cancel` row naming its cause: the failing checks, the head,
/// the PR, and each run's result.
pub(crate) fn cancel_in_flight(
    gh_bin: &str,
    cwd: &Path,
    pr: u64,
    head: &str,
) -> Option<Vec<String>> {
    let short: String = head.chars().take(8).collect();
    // pr.cancel_red_run = false leaves the runs in flight everywhere: the
    // wait reads the red verdict as final and cancels nothing (a sharded
    // suite wants every failure from its one run). The gate sits above the
    // staleness read, so even a rerun-in-progress red exits red.
    let cancel_red_run = crate::agents_config::config_lookup(cwd, &["pr", "cancel_red_run"])
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !cancel_red_run {
        return Some(vec![format!(
            "wait: red at {short}; pr.cancel_red_run=false leaves the runs in flight"
        )]);
    }
    if head.is_empty() {
        return Some(vec![
            "wait: red, but the status names no head; no run cancelled".into(),
        ]);
    }
    let rows = match crate::pr_push::read_checks_rows(gh_bin, cwd, head) {
        Ok(rows) => rows,
        Err(e) => {
            return Some(vec![format!(
                "wait: cancel FAILED: cannot read the runs on {short}: {e}"
            )])
        }
    };
    // The evals gate is not CI: its row carries no run link, so left in the
    // failed set it can never read stale-attempt, and a gate red beside a
    // stale CI red would cancel the fresh rerun answering that red.
    // An outside hold is the same class: its red is external truth the PR
    // cannot heal, so left in the failed set it would cancel the PR's own
    // fresh runs over a defect no commit fixes. An outside-hold-only red
    // then reads stale (failed empty -> red_is_stale), and the wait keeps
    // polling until the deploy clears it. A real failing check beside the
    // hold still cancels as today. The shared rule with the push's
    // in-flight read lives in `cancellable_rows`.
    let rows = crate::pr_status::cancellable_rows(rows);
    let latest = crate::check_supersession::latest_per_name(&Value::Array(rows));
    // The hold-only shape: the aggregate is red because the pr-hold label
    // is live, and no row beside it failed. The red is the hold itself, the
    // same class as an outside hold's red one filter up, so cancel nothing
    // and keep polling until the hold releases; the reruns a cancel bought
    // would red again on the same label. A real failing row beside the
    // hold, or a label read that answers empty, cancels as today.
    if hold_only_red(latest.as_array().map(Vec::as_slice).unwrap_or(&[]))
        && pr_hold_label_live(gh_bin, cwd, pr)
    {
        return None;
    }
    // An unreadable runs listing proves nothing stale: the law's cancel runs.
    let listing: Vec<Value> = crate::pr_push::gh_api_pages_conditional(
        gh_bin,
        cwd,
        &format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={head}&per_page=100"),
    )
    .unwrap_or_default()
    .iter()
    .filter_map(|page| page.get("workflow_runs").and_then(Value::as_array))
    .flatten()
    .cloned()
    .collect();
    if red_is_stale(
        latest.as_array().map(Vec::as_slice).unwrap_or(&[]),
        &listing,
    ) {
        return None;
    }
    let pending: Vec<&Value> = latest
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            !matches!(
                row.get("bucket").and_then(Value::as_str).unwrap_or(""),
                "pass" | "fail" | "skipping" | "cancel"
            )
        })
        .collect();
    let runs = crate::pr_push::pending_run_ids(&pending);
    if runs.is_empty() {
        return Some(vec![format!(
            "wait: red at {short}; no run in flight to cancel"
        )]);
    }
    let cause: Vec<&str> = latest
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row.get("bucket").and_then(Value::as_str) == Some("fail"))
                .filter_map(|row| row.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let mut results = Vec::new();
    let mut cancelled = Vec::new();
    for id in &runs {
        match crate::pr_push::cancel_run(gh_bin, cwd, *id, "ci_wait") {
            Ok(_) => {
                results.push(json!({"run": id, "ok": true}));
                cancelled.push(format!("wait: red at {short}; cancelled run {id}"));
            }
            Err(e) => {
                results.push(json!({"run": id, "ok": false, "error": e}));
                cancelled.push(format!(
                    "wait: red at {short}; cancel of run {id} FAILED: {e}"
                ));
            }
        }
    }
    journal_cancel(cwd, pr, head, &cause, &results);
    Some(cancelled)
}

/// One `ci_wait_cancel` row per cancel action, best-effort: the stderr
/// lines reach the waiting session only, and the journal is the record
/// every other session reads. A missing home (tests) or a failed write
/// leaves the cancel itself untouched.
fn journal_cancel(cwd: &Path, pr: u64, head: &str, cause: &[&str], results: &[Value]) {
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        return;
    };
    let row = json!({
        "slug": crate::pr_status::cache::git_slug(cwd),
        "pr": pr,
        "head": head,
        "cause": cause,
        "runs": results,
        "session_id": crate::claims::resolve_identity().0,
    });
    let path = crate::daemon::global_events_path(&home);
    if let Err(e) = crate::events::EventEmitter::new(path, "rust").emit("ci_wait_cancel", &row) {
        eprintln!("wait: the ci_wait_cancel row did not land ({e})");
    }
}

/// Reviews on `pr` via one conditional REST read; None when the read fails
/// (no-answer, never "no new review"). per_page=100 is load-bearing: gh api
/// fetches ONE page, so at the default 30 the count saturates and a 31st
/// review can never wake the wait. No `--jq`: the conditional door refuses
/// jq'd shapes, so the plain array read rides gh_etag and a 304 serves the
/// stored body at no core cost on every unchanged tick.
fn review_count<P: GhProbe>(probe: &P, cwd: &Path, slug: &str, pr: u64) -> Option<i64> {
    let args = vec![
        "api".to_string(),
        format!("repos/{slug}/pulls/{pr}/reviews?per_page=100"),
    ];
    let (ok, stdout, _) = probe.run_gh(cwd, &args).ok()?;
    if !ok {
        return None;
    }
    let body: Value = serde_json::from_str(&stdout).ok()?;
    body.as_array().map(|a| a.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    fn staged_poll<'a>(
        ticks: Vec<(i32, Value)>,
    ) -> impl FnMut() -> (i32, Value, Vec<String>, usize) + 'a {
        let mut it = ticks.into_iter();
        move || {
            let (code, payload) = it.next().unwrap_or((2, json!({"verdict": "pending"})));
            (code, payload, Vec::new(), 1)
        }
    }

    #[test]
    fn a_conflicting_pr_exits_five_with_the_rebase_receipt() {
        let payloads = vec![(
            2,
            json!({
                "verdict": "pending",
                "settled": false,
                "mergeable": "CONFLICTING",
                "pr_state": "OPEN",
                "head": "abcdef1234567890",
            }),
        )];
        let (code, _stdout, stderr) = wait_status(
            9,
            "settled",
            None,
            1800.0,
            60.0,
            staged_poll(payloads),
            |_| Some(Vec::new()),
            |_| {},
            || 0.0,
        );
        assert_eq!(code, 5, "the rebase receipt exit");
        assert!(stderr.contains("is CONFLICTING at abcdef12"), "{stderr}");
        assert!(stderr.contains("Integrate the base"), "{stderr}");
    }

    #[test]
    fn a_red_verdict_cancels_the_head_runs_and_wakes_at_once() {
        for until in ["settled", "red"] {
            let t = Cell::new(0.0);
            let heads = std::cell::RefCell::new(Vec::new());
            let payloads: Vec<(i32, Value)> = (0..100)
                .map(|_| {
                    (
                        1,
                        json!({"verdict": "red", "settled": false, "head": "abcdef1234"}),
                    )
                })
                .collect();
            let (code, stdout, stderr) = wait_status(
                9,
                until,
                None,
                1800.0,
                10.0,
                staged_poll(payloads),
                |p| {
                    heads.borrow_mut().push(p["head"].clone());
                    Some(vec![
                        "wait: red at abcdef12; cancel of run 7 FAILED: 403".into()
                    ])
                },
                |s| t.set(t.get() + s),
                || t.get(),
            );
            assert_eq!(code, 1, "{until}: the red verdict's code");
            assert!(stdout.contains("\"verdict\":\"red\""), "{stdout}");
            assert_eq!(t.get(), 0.0, "{until}: the first red tick wakes");
            assert_eq!(*heads.borrow(), vec![json!("abcdef1234")]);
            assert!(stderr.contains("cancel of run 7 FAILED: 403"), "{stderr}");
        }
    }

    #[test]
    fn red_is_stale_reads_each_failure_against_its_runs_attempt_start() {
        let fail = |run: u64, done: &str| {
            json!({
                "bucket": "fail",
                "link": format!("https://github.com/o/r/actions/runs/{run}/job/1"),
                "completedAt": done,
            })
        };
        let runs = vec![
            json!({"id": 7, "status": "completed", "run_started_at": "2026-10-22T17:13:00Z"}),
            json!({"id": 8, "status": "completed", "run_started_at": "2026-10-22T16:00:00Z"}),
        ];
        let mut queued = runs.clone();
        queued[0]["status"] = json!("queued");
        let pending =
            json!({"bucket": "pending", "link": "https://github.com/o/r/actions/runs/7/job/2"});
        assert!(
            red_is_stale(&[fail(7, "2026-10-22T16:59:00Z"), pending.clone()], &runs),
            "run 7 restarted after its job failed: the old attempt's red"
        );
        assert!(
            !red_is_stale(&[fail(8, "2026-10-22T16:30:00Z"), pending.clone()], &runs),
            "a failure inside the current attempt is live: the law's cancel runs"
        );
        assert!(
            !red_is_stale(
                &[
                    fail(7, "2026-10-22T16:59:00Z"),
                    fail(8, "2026-10-22T16:30:00Z")
                ],
                &runs
            ),
            "one live failure keeps the head red"
        );
        assert!(
            !red_is_stale(&[fail(7, "2026-10-22T16:59:00Z")], &runs),
            "nothing in flight: the old failure is final, as after a partial rerun"
        );
        assert!(
            red_is_stale(&[fail(7, "2026-10-22T16:59:00Z")], &queued),
            "a queued rerun with no job row yet is in flight"
        );
        assert!(
            !red_is_stale(&[fail(7, ""), pending.clone()], &runs),
            "no completion time is never stale"
        );
        assert!(
            !red_is_stale(&[fail(9, "2026-10-22T16:59:00Z"), pending.clone()], &runs),
            "an unlisted run is never stale"
        );
        assert!(
            red_is_stale(&[json!({"bucket": "cancel"}), pending.clone()], &runs),
            "a red made only of cancelled checks is no verdict: nothing cancels"
        );
        assert!(
            red_is_stale(&[json!({"bucket": "pass"})], &runs),
            "no live failure: the red was a cached row's"
        );
        let ghost_runs = vec![
            json!({"id": 7, "status": "completed", "run_started_at": "2026-10-22T17:13:00Z"}),
            json!({"id": 8, "status": "completed", "conclusion": "cancelled", "run_started_at": "2026-10-22T16:00:00Z"}),
        ];
        assert!(
            red_is_stale(
                &[fail(8, "2026-10-22T16:30:00Z"), pending.clone()],
                &ghost_runs
            ),
            "a fail row of a run that concluded cancelled is a ghost: the rerun is not cancelled"
        );
        assert!(
            red_is_stale(&[fail(8, "2026-10-22T16:30:00Z")], &ghost_runs),
            "a ghost-only red reads pending, never red-cancel"
        );
        assert!(
            !red_is_stale(
                &[
                    fail(8, "2026-10-22T16:30:00Z"),
                    fail(7, "2026-10-22T17:20:00Z"),
                    pending.clone()
                ],
                &ghost_runs
            ),
            "a live failure beside a ghost still cancels"
        );
    }

    /// The pinned wait: armed on the sha just pushed. A live read at any
    /// other head is exit 6 at once, naming the mismatch; a merged PR can
    /// never answer settled for the named commit again.
    #[test]
    fn a_pinned_wait_exits_six_the_moment_the_head_differs() {
        let t = Cell::new(0.0);
        let payloads = vec![
            (2, json!({"verdict": "pending", "settled": false})),
            (
                0,
                json!({
                    "verdict": "green",
                    "settled": true,
                    "head": "37deafc2abcdef",
                    "pr_state": "OPEN",
                    "ready": true,
                    "ready_blockers": [],
                }),
            ),
        ];
        let (code, stdout, stderr) = wait_status(
            9,
            "settled",
            Some("aaaaaaaa1234aaaaaaaa1234aaaaaaaa1234"),
            1800.0,
            10.0,
            staged_poll(payloads),
            |_| Some(Vec::new()),
            |s| t.set(t.get() + s),
            || t.get(),
        );
        assert_eq!(code, 6, "the pinned mismatch exit");
        assert!(stdout.contains("\"head_mismatch\""), "{stdout}");
        assert!(stdout.contains("\"settled\":false"), "{stdout}");
        assert!(
            stderr.contains("head_mismatch: PR head is 37deafc2abcd"),
            "{stderr}"
        );
        assert!(stderr.contains("expected aaaaaaaa1234"), "{stderr}");
    }

    /// A fake gh: one live failing check (node-closure completed after its
    /// run started), one run in flight, and a cancel POST answering ok.
    fn cancel_stub_gh(dir: &std::path::Path) -> std::path::PathBuf {
        crate::write_exec_stub(
            dir,
            "gh",
            r#"#!/bin/sh
for a in "$@"; do case "$a" in
  *check-runs*)
    echo '{"check_runs":[
      {"id":201,"name":"node-closure","status":"completed","conclusion":"failure","started_at":"2026-11-01T03:00:00Z","completed_at":"2026-11-01T03:05:00Z","html_url":"https://github.com/o/r/actions/runs/301/job/1","check_suite":{"id":11}},
      {"id":202,"name":"rust-ci","status":"in_progress","conclusion":null,"started_at":"2026-11-01T03:06:00Z","completed_at":null,"html_url":"https://github.com/o/r/actions/runs/302/job/2","check_suite":{"id":12}}]}'
    exit 0 ;;
  *head_sha=*)
    echo '{"total_count":2,"workflow_runs":[{"id":301,"name":"cli-ci","status":"completed","conclusion":"failure","created_at":"2026-11-01T03:00:00Z","run_started_at":"2026-11-01T03:00:00Z"},{"id":302,"name":"rust-ci","status":"in_progress","conclusion":null,"created_at":"2026-11-01T03:06:00Z","run_started_at":"2026-11-01T03:06:00Z"}]}'
    exit 0 ;;
  *commits/*/status*)
    echo '{"statuses":[]}'
    exit 0 ;;
  *cancel*)
    exit 0 ;;
esac; done
echo '{}'
exit 1
"#,
        )
    }

    /// A fake gh for the hold gate: the ci-required aggregate red (its row
    /// keyed by the workflow file, not the job name), one live rust-ci run,
    /// and the labels answer the parameter picks. `extra_fail` adds a real
    /// failing row beside the aggregate red.
    fn hold_gate_stub(dir: &std::path::Path, labels: &str, extra_fail: bool) -> std::path::PathBuf {
        let (extra_checks, extra_runs) = if extra_fail {
            (
                r#"{"id":203,"name":"node-closure","status":"completed","conclusion":"failure","started_at":"2026-11-01T03:00:00Z","completed_at":"2026-11-01T03:05:00Z","html_url":"https://github.com/o/r/actions/runs/303/job/3","check_suite":{"id":13}},"#,
                r#"{"id":303,"name":"node-closure","status":"completed","conclusion":"failure","created_at":"2026-11-01T03:00:00Z","run_started_at":"2026-11-01T03:00:00Z","check_suite_id":13,"path":".github/workflows/node-closure.yml"},"#,
            )
        } else {
            ("", "")
        };
        let script = r#"#!/bin/sh
for a in "$@"; do case "$a" in
  *check-runs*)
    echo '{"check_runs":[
      @EXTRA_CHECKS@
      {"id":201,"name":"wait for every run at the head","status":"completed","conclusion":"failure","started_at":"2026-11-01T03:00:00Z","completed_at":"2026-11-01T03:05:00Z","html_url":"https://github.com/o/r/actions/runs/301/job/1","check_suite":{"id":11}},
      {"id":202,"name":"rust-ci","status":"in_progress","conclusion":null,"started_at":"2026-11-01T03:06:00Z","completed_at":null,"html_url":"https://github.com/o/r/actions/runs/302/job/2","check_suite":{"id":12}}]}'
    exit 0 ;;
  *head_sha=*)
    echo '{"total_count":2,"workflow_runs":[
      @EXTRA_RUNS@
      {"id":301,"name":"ci-required","status":"completed","conclusion":"failure","created_at":"2026-11-01T03:00:00Z","run_started_at":"2026-11-01T03:00:00Z","check_suite_id":11,"path":".github/workflows/ci-required.yml"},
      {"id":302,"name":"rust-ci","status":"in_progress","conclusion":null,"created_at":"2026-11-01T03:06:00Z","run_started_at":"2026-11-01T03:06:00Z","check_suite_id":12,"path":".github/workflows/rust-ci.yml"}]}'
    exit 0 ;;
  *commits/*/status*)
    echo '{"statuses":[]}'
    exit 0 ;;
  *issues/*/labels*)
    echo '@LABELS@'
    exit 0 ;;
  *cancel*)
    exit 0 ;;
esac; done
echo '{}'
exit 1
"#
        .replace("@EXTRA_CHECKS@", extra_checks)
        .replace("@EXTRA_RUNS@", extra_runs)
        .replace("@LABELS@", labels);
        crate::write_exec_stub(dir, "gh", &script)
    }

    /// A journaled cancel: one `ci_wait_cancel` row names the PR, the head,
    /// the failing checks that caused it, and each run's result. The stderr
    /// lines reach one session; the journal is the record the rest of the
    /// fleet reads.
    #[test]
    fn a_cancel_journals_a_row_that_names_its_cause() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The env value IS the agents root; the journal lands at its parent.
        let agents = home.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let _home =
            crate::claims::EnvVarGuard::set(crate::paths::HOME_ENV, agents.to_str().unwrap());
        let gh = cancel_stub_gh(dir.path());
        let lines = cancel_in_flight(gh.to_str().unwrap(), dir.path(), 4254, "abc123").unwrap();
        assert!(
            lines.iter().all(|l| l.contains("cancelled run 302")),
            "{lines:?}"
        );
        // The store lands the row: read it back through the sanctioned
        // reader instead of the flat file the store replaces.
        let journal = home.path().join("events.jsonl");
        let query =
            crate::event_store::EventQuery::of_types(&["ci_wait_cancel", "ci_cancel_intent"]);
        let text = crate::event_store::journal_text_checked(&journal, &query).unwrap();
        let rows: Vec<Value> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        // Every cancel POST is preceded by a row naming who asked.
        let intent = rows
            .iter()
            .find(|r| r["type"] == "ci_cancel_intent")
            .expect("the intent row landed");
        assert_eq!(intent["data"]["run"], 302);
        assert_eq!(intent["data"]["actor"], "ci_wait");
        assert!(intent["data"]["pid"].is_u64(), "{intent}");
        let row = rows
            .iter()
            .find(|r| r["type"] == "ci_wait_cancel")
            .expect("the cancel row landed");
        assert_eq!(row["data"]["pr"], 4254);
        assert_eq!(row["data"]["head"], "abc123");
        assert_eq!(row["data"]["cause"], json!(["node-closure"]));
        assert_eq!(row["data"]["runs"], json!([{"run": 302, "ok": true}]));
        // The resolver answers None in a bare test process; the row still
        // carries the field.
        assert!(
            row["data"].get("session_id").is_some(),
            "the row carries the session field: {row}"
        );
    }

    /// The hold gate: a red whose only failed row is the ci-required
    /// aggregate with the pr-hold label live cancels nothing and journals
    /// nothing (None keeps the wait polling). The admit arms cancel as
    /// today: the same red without the label, and a real failing row
    /// beside the aggregate red.
    #[test]
    fn a_hold_only_ci_required_red_cancels_nothing_and_a_real_row_beside_the_hold_still_cancels() {
        let home = tempfile::tempdir().unwrap();
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let agents = home.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let _home =
            crate::claims::EnvVarGuard::set(crate::paths::HOME_ENV, agents.to_str().unwrap());
        let journal = home.path().join("events.jsonl");
        let query = crate::event_store::EventQuery::of_types(&["ci_cancel_intent"]);

        // Withhold: hold-only red, label live.
        let dir = tempfile::tempdir().unwrap();
        let gh = hold_gate_stub(dir.path(), r#"[{"name":"pr-hold"}]"#, false);
        let lines = cancel_in_flight(gh.to_str().unwrap(), dir.path(), 4254, "abc123");
        assert!(lines.is_none(), "{lines:?}");
        assert!(!journal.exists(), "the hold-only red wrote no cancel row");

        // Admit, label absent: the same red with no label cancels as today.
        let dir = tempfile::tempdir().unwrap();
        let gh = hold_gate_stub(dir.path(), r#"[]"#, false);
        let lines = cancel_in_flight(gh.to_str().unwrap(), dir.path(), 4254, "abc123").unwrap();
        assert!(
            lines.iter().all(|l| l.contains("cancelled run 302")),
            "{lines:?}"
        );

        // Admit, real row beside the hold: a node-closure failure beside
        // the aggregate red cancels even with the label live.
        let dir = tempfile::tempdir().unwrap();
        let gh = hold_gate_stub(dir.path(), r#"[{"name":"pr-hold"}]"#, true);
        let lines = cancel_in_flight(gh.to_str().unwrap(), dir.path(), 4254, "abc123").unwrap();
        assert!(
            lines.iter().all(|l| l.contains("cancelled run 302")),
            "{lines:?}"
        );
        let text = crate::event_store::journal_text_checked(&journal, &query).unwrap();
        assert!(text.contains("\"ci_cancel_intent\""), "{text}");
    }

    #[test]
    fn a_bare_wait_in_a_checkout_with_no_pr_refuses_instead_of_polling_pr_zero() {
        // PR 0 404s on every tick; the old loop burned its whole timeout on
        // the same error. The bare spelling now resolves the branch's PR once
        // up front, and a checkout that names no PR refuses with the explicit
        // spelling before the first tick.
        let dir = std::env::temp_dir().join(format!("fno-pr-wait-bare-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let payload = json!({
            "cwd": dir.to_string_lossy(),
            "pr": 0,
            "until": "settled",
            "timeout": "2s",
            "interval": "1s",
        });
        let (code, stdout, stderr) = run_wait(&payload);
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("fno do pr wait <number>"), "{stderr}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
