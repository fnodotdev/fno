//! The watch lease: what a `<watching>` tag is allowed to idle on, and for
//! how long. One question, so one module - the four answers below were four
//! loose helpers in the middle of the stop gate.

/// Slack added beyond the declared watch window so the claim lease outlives the
/// agent's watcher: a watcher that fires right at its timeout must not
/// race claim expiry.
use crate::harness::Harness;

pub(super) const WATCH_SLACK_MS: i64 = 12 * 60_000;

/// How long a build park lasts before the daemon wakes the session anyway:
/// the longest one agent cargo holds an admission claim by default.
const BUILD_PARK_MS: i64 = 60 * 60_000;

/// Park a session whose cargo waits at an admission door. The stop gate
/// already lets that session stop. This records a `loop_check_watch_idle`
/// row with blocker `build` that names the parked cargo, so the daemon's
/// expiry arm wakes the session when that cargo exits, or at the deadline,
/// and the park is checkable by its cargo pid. It renews the node claim
/// lease for the same window, so a parked session keeps its node. A session
/// with no parked cargo or no target manifest records nothing.
pub(super) fn record_build_park(parsed: &super::LoopCheckArgs) {
    let Some(wait) = crate::test_run::build_wait(&parsed.cwd) else {
        return;
    };
    let Ok(manifest) = std::fs::read_to_string(&parsed.state_path) else {
        return;
    };
    let Some(session_id) = super::parse_manifest(&manifest).and_then(|m| m.session_id) else {
        return;
    };
    if let Some((key, holder)) = claim_pair(&manifest) {
        let _ = crate::claims::renew(&key, &holder, BUILD_PARK_MS + WATCH_SLACK_MS, None);
    }
    let project_events = parsed
        .events_path
        .clone()
        .unwrap_or_else(|| crate::paths::events_path(&parsed.cwd));
    let global_events = parsed
        .global_events_path
        .clone()
        .unwrap_or_else(super::default_global_events_path);
    let row = serde_json::json!({
        "session_id": session_id,
        "pr": null,
        "blocker": "build",
        "reason": "build",
        "expires_at_ms": crate::claims::now_ms() + BUILD_PARK_MS,
        "lease_ms": BUILD_PARK_MS + WATCH_SLACK_MS,
        "build_cargo_pid": wait.cargo_pid,
        "build_since_ms": wait.since_ms,
        "build_holder": wait.holder,
        "harness": super::scan_manifest_field(&manifest, "harness"),
        "cwd": &parsed.cwd,
    });
    if let Err(e) = super::fire_history::emit_to_both_checked(
        &project_events,
        &global_events,
        "loop_check_watch_idle",
        row,
    ) {
        eprintln!("loop-check: build park not recorded ({e}); no daemon wake is armed");
    }
}

/// Lease window for an idle watch: the declared timeout clamped to [5m, 15m]
/// (the ci-push-cadence law checks every 15 minutes; never trust the tag for
/// an unbounded hold) plus slack. Defaults to 15m when the tag omits or
/// mangles `timeout`, giving the ~27m default lease.
pub(super) fn watch_window_ms(timeout: Option<&str>) -> i64 {
    watch_timeout_ms(timeout) + WATCH_SLACK_MS
}

/// The actual watch lifetime, separate from the longer claim lease window.
pub(super) fn watch_timeout_ms(timeout: Option<&str>) -> i64 {
    timeout
        .and_then(crate::claims::parse_ttl_ms)
        .unwrap_or(15 * 60_000)
        .clamp(5 * 60_000, 15 * 60_000)
}

pub(super) fn watch_expiry_ms(timeout: Option<&str>, now_ms: i64) -> i64 {
    now_ms.saturating_add(watch_timeout_ms(timeout))
}

/// What a `<watching>` tag says the session waits on: the blocker class the
/// idle event records, and the PR number only when the tag names a real one.
/// `pr="0"`, `pr="pending"` and an absent pr all read as no PR. The tag's own
/// `reason=`/`pr=` attributes are the only source here (the idle verifies
/// nothing), so the blocker is only as trustworthy as the agent's tag.
pub(super) fn watch_target(reason: &str, pr: Option<&str>) -> (&'static str, Option<i64>) {
    let blocker = match reason {
        "ci" => "ci",
        "review" => "review",
        "merge_slot" => "merge_slot",
        "merge_hold" => "merge_hold",
        "outside_hold" => "outside_hold",
        "local" => "local",
        _ => "unknown",
    };
    (
        blocker,
        pr.and_then(|s| s.trim().parse::<i64>().ok())
            .filter(|n| *n > 0),
    )
}

/// The only runtime text a session with no PR sees teaching the `<watching>`
/// tag: the Step-5 continue message passed to the inbox nudge.
pub(super) const CONTINUE_WORKING: &str = "continue working; no completion signal. If you are \
only waiting with nothing to do, arm a harness-tracked watcher with a hard timeout and end your \
turn with the tag. A PR wait: background Bash `fno do pr wait <N> --until settled \
--timeout=15m` (REST through the coalescing cache, 60s interval, never `gh pr checks --watch`, \
which spends the shared GraphQL quota; a review wait is `--until review`), then `<watching \
reason=\"ci|review\" pr=\"<N>\" timeout=\"15m\">`. A local run (a test suite, a build, a review \
fork) is its own background task; include its actual harness task id \
(e.g. task-123) in `<watching reason=\"local\" task_id=\"task-123\" timeout=\"15m\">` with no \
pr: pr is a real PR number or left out, never 0. The session idles until the watcher exits \
instead of re-waking every tick.";

/// The harnesses whose `<watching>` idle may park. One set, two readers: the
/// lease gate and the daemon's settle arm, so a harness the lease admits is
/// exactly the harness the settle arm wakes.
pub(crate) const IDLE_HARNESSES: [&str; 6] = ["claude", "codex", "opencode", "pi", "agy", "gemini"];

/// Whether a session's harness + substrate can park on a `<watching>` idle.
/// The supported interactive harnesses can be resumed through the daemon's
/// expiry arm. Loop-run children exit on allow, and unknown or unsupported
/// harnesses stay on the blocking path rather than idling without a routed wake.
pub(crate) fn harness_can_idle(author_harness: Option<&str>, is_loop_run_child: bool) -> bool {
    // The daemon cannot re-wake a session inside a sandbox, so a sandbox
    // stop never parks or idles: it blocks.
    author_harness.is_some_and(|h| IDLE_HARNESSES.contains(&h))
        && !is_loop_run_child
        && crate::sandbox::origin().is_none()
}

/// US6a: the codex watch registration address. `Some` only for a codex
/// session: the manifest's `codex_thread_id` (what `fno do target init`
/// recorded), falling back to the session id when init recorded none. A
/// non-codex session answers None (no codex gate, no extra event fields).
pub(crate) fn codex_watch_target(
    author_harness: Option<&str>,
    manifest: &str,
    session_id: &str,
) -> Option<String> {
    if author_harness.map(Harness::parse) != Some(Harness::Codex) {
        return None;
    }
    Some(
        super::scan_manifest_field(manifest, "codex_thread_id")
            .filter(|id| !id.is_empty() && !id.eq_ignore_ascii_case("null"))
            .unwrap_or_else(|| session_id.to_string()),
    )
}

/// AC4-EDGE: the daemon cannot reach the thread (absent daemon, or up daemon
/// with the thread unloaded). Today's block behavior is the status quo, so
/// the refusal names the channel, not a watcher ritual codex cannot run.
pub(crate) const CODEX_UNROUTABLE_REFUSAL: &str = "watching ignored: the fno daemon cannot reach \
this codex thread (no app-server daemon, or thread not loaded), so no watcher would wake this \
park. Stop instead of idling; when the daemon is up again the next stop may idle.";

/// The `loop_check_watch_idle` event IS the daemon's watch
/// registration, so a failed write refuses the idle.
pub(crate) const EMIT_FAILED_REFUSAL: &str = "watching ignored: the watch registration \
(loop_check_watch_idle) failed to write, so no watcher is registered. Do not idle on this \
tag; the next stop retries the write.";

pub(crate) fn watching_harness_refusal(
    author_harness: Option<&str>,
    is_loop_run_child: bool,
) -> String {
    if is_loop_run_child {
        "watching ignored: loop-run child cannot idle".to_string()
    } else {
        format!(
            "watching ignored: harness {} cannot idle and cannot wake itself. Do not arm a \
watcher or re-emit the tag - stop instead. claude parks on its own harness-tracked watcher; \
codex parks only when the fno daemon can inject (loop_check_watch_idle routability gate).",
            author_harness.unwrap_or("unknown")
        )
    }
}

/// The longest a failed-read park lasts, quota or gh blip: past it the
/// daemon re-wakes the session and the gate reads again.
pub(super) const PARK_MAX_MS: i64 = 2 * 60 * 60_000;

/// The first park for a failed gh read. Each consecutive failed fire doubles
/// it up to [`PARK_MAX_MS`], so an outage costs one wake per window.
const GH_READ_PARK_MS: i64 = 180_000;

/// The park for a failed gh read after `streak` failed fires in a row.
pub(super) fn gh_read_park_ms(streak: u32) -> i64 {
    GH_READ_PARK_MS
        .saturating_mul(1_i64 << streak.min(16))
        .min(PARK_MAX_MS)
}

/// Arm the daemon re-wake for a session the stop gate parks on a failed
/// read: renew the node claim over the window and write the
/// `loop_check_watch_idle` row the expiry arm wakes from. The caller writes
/// its `loop_check` fire row FIRST: a later non-watching fire row marks the
/// watch superseded (`is_current_watch`) and the daemon would never wake it.
#[allow(clippy::too_many_arguments)]
pub(super) fn arm_failed_read_park(
    blocker: &str,
    park_ms: i64,
    expires_at_ms: i64,
    manifest: &str,
    session_id: &str,
    cwd: &std::path::Path,
    project_events: &std::path::Path,
    global_events: &std::path::Path,
) {
    let lease_ms = park_ms + 2 * WATCH_SLACK_MS;
    if let Some((key, holder)) = claim_pair(manifest) {
        if let Err(e) = crate::claims::renew(&key, &holder, lease_ms, None) {
            eprintln!("loop-check: {blocker} park could not renew {key} ({e})");
        }
    }
    // `lease_ms` is a required field: the store refuses a row without it.
    let idle_row = serde_json::json!({
        "session_id": session_id,
        "pr": null,
        "blocker": blocker,
        "reason": blocker,
        "expires_at_ms": expires_at_ms,
        "lease_ms": lease_ms,
        "harness": super::scan_manifest_field(manifest, "harness"),
        "cwd": cwd,
    });
    if let Err(e) = super::fire_history::emit_to_both_checked(
        project_events,
        global_events,
        "loop_check_watch_idle",
        idle_row,
    ) {
        eprintln!("loop-check: {blocker} park not recorded ({e}); no daemon wake is armed");
    }
}

/// The claim pair this session recorded at init, if it recorded one.
///
/// `fno do target init` writes neither field when the node was already held by
/// another session, and the manifest is write-once. So an absent pair is
/// PERMANENT for the life of the session: no lease here can ever renew.
pub(super) fn claim_pair(manifest: &str) -> Option<(String, String)> {
    super::scan_manifest_field(manifest, "target_claim_key")
        .zip(super::scan_manifest_field(manifest, "target_claim_holder"))
}

/// Said instead of the generic renewal refusal when there is no claim at all.
/// The generic one reads as transient, so a reader believes another watcher
/// will help, arms one on every stop, and never idles once. The heal line
/// names the exact working shape because a worker burned three refused
/// acquires on it (measured 2026-10-30): an unprefixed holder lands the claim
/// but the manifest refresh refuses it, so the blocked line survives and the
/// gate keeps reading no claim.
pub(super) const NO_CLAIM_REFUSAL: &str =
    "watching ignored: this session recorded no node claim at init, so no watch lease can \
ever renew, and arming another watcher will not change that. The manifest is write-once, \
so rerunning `fno do target start` cannot restore the claim. If `fno agent claim status \
node:<id>` shows the node held by `spawn-handover:<this worker>` (live or suspect), the \
heal is `fno agent claim acquire node:<id> --holder target-session:<session id> \
--handover-from spawn-handover:<worker> --pid-unavailable --ttl <lease>`; the \
`target-session:` prefix is required, because the manifest refresh refuses an unprefixed \
holder, and the handover lands the claim lines and drops the blocked line. Once the \
window is gone no command heals the manifest. Hand the PR to a session that holds the \
claim.";

/// The lead of the arm-and-tag hint. One literal, shared by the writer (the
/// Step-5 continue message) and the idle-refusal tests, so the cut can never
/// drift off the sentence it cuts.
pub(super) const ARM_HINT_LEAD: &str = " Arm a harness-tracked watcher";

/// The arm-and-tag hint's lead, shared with the writer: cutting a hint means
/// cutting from this exact sentence on.
pub(super) fn without_arm_hint(reason: &str) -> String {
    match reason.find(ARM_HINT_LEAD) {
        Some(i) => reason[..i].trim_end().to_string(),
        None => reason.to_string(),
    }
}

/// Whether a refusal text is permanent for the session: re-arming a watcher
/// cannot change it, so the block drops the arm hint instead of contradicting
/// itself inside one message.
pub(super) fn refusal_is_permanent(reason: &str) -> bool {
    reason == NO_CLAIM_REFUSAL || reason.starts_with(PERMANENT_REFUSAL_LEAD)
}

/// Attach the refusal's cause class to a loop_check row.
pub(super) fn attach_watch_refusal(event: &mut serde_json::Value, kind: Option<&'static str>) {
    if let Some(kind) = kind {
        event["watch_refusal"] = serde_json::Value::String(kind.to_string());
    }
}

/// Why a watch-lease renewal declined. The first three are permanent
/// for this session: arming another watcher cannot change them. `contended`
/// (a peer held the recovery mutex, or the record answered nothing) and
/// `write_failed` can succeed on the next stop.
#[derive(Debug)]
pub(super) enum RenewCause {
    /// The claim lockfile is missing (released or reaped).
    Gone,
    /// The lockfile's holder differs from the manifest's recorded holder.
    HeldByOther(String),
    /// The holder matches but the status verdict reads stale.
    Stale,
    /// The holder matches, the verdict reads live/suspect, and renewal still
    /// declined: a peer held the recovery mutex.
    Contended,
    /// `renew` returned Err.
    WriteFailed,
}

impl RenewCause {
    /// The cause string. schema.yaml keeps the `watch_refusal` enum for old
    /// journals; nothing emits the event since the inline refusal landed.
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            RenewCause::Gone => "gone",
            RenewCause::HeldByOther(_) => "held_by_other",
            RenewCause::Stale => "stale",
            RenewCause::Contended => "contended",
            RenewCause::WriteFailed => "write_failed",
        }
    }

    pub(super) fn is_permanent(&self) -> bool {
        matches!(
            self,
            RenewCause::Gone | RenewCause::HeldByOther(_) | RenewCause::Stale
        )
    }
}

/// The lead shared by every permanent-cause refusal. The transient refusals
/// ("watch lease could not be renewed", harness, not-async) never carry it.
const PERMANENT_REFUSAL_LEAD: &str = "watching ignored: this session's watch lease is dead:";

/// The refusal for a cause, or `None` for a transient one: the caller keeps
/// the generic text, because a retry can succeed.
pub(super) fn renewal_refusal(cause: &RenewCause) -> Option<String> {
    let remedy = "Get the claim back with `fno do target start <node>` from inside this \
worktree, then resume; or hand the PR to a session that holds the claim.";
    match cause {
        RenewCause::Gone => Some(format!(
            "{PERMANENT_REFUSAL_LEAD} the node claim lockfile is gone (released or \
reaped). Arming another watcher will not change that. {remedy}"
        )),
        RenewCause::HeldByOther(holder) => Some(format!(
            "{PERMANENT_REFUSAL_LEAD} the node claim is held by {holder}, not this \
session. Arming another watcher will not change that. {remedy}"
        )),
        RenewCause::Stale => Some(format!(
            "{PERMANENT_REFUSAL_LEAD} the claim's holder reads dead (stale). Arming \
another watcher will not change that. {remedy}"
        )),
        RenewCause::Contended | RenewCause::WriteFailed => None,
    }
}

/// Why `renew` did not answer Ok(true) for this session's own claim pair
///. Reads the claim once and applies the same status verdict
/// `fno agent claim status` prints, so a refusal names the answer the
/// operator would see - never a second liveness opinion. `renew_error` is
/// renew's Err payload when it errored; `root` mirrors renew's own root
/// argument (production passes None).
pub(super) fn renew_cause(
    key: &str,
    holder: &str,
    renew_error: Option<&str>,
    root: Option<&std::path::Path>,
) -> RenewCause {
    if renew_error.is_some() {
        return RenewCause::WriteFailed;
    }
    let path = match crate::claims::claim_path(key, root) {
        Ok(path) => path,
        Err(_) => return RenewCause::WriteFailed,
    };
    match crate::claims::read_claim_file(&path) {
        Err(crate::claims::ReadError::GoneAway) => RenewCause::Gone,
        Err(crate::claims::ReadError::Corrupted(_)) => RenewCause::Contended,
        Ok(rec) if rec.holder != holder => RenewCause::HeldByOther(rec.holder),
        Ok(rec) => {
            if rec
                .expires_at
                .is_some_and(|expires| expires <= crate::claims::now_ms())
                || crate::claim_commands::status_verdict(&rec).0 == crate::claims::ClaimState::Stale
            {
                RenewCause::Stale
            } else {
                RenewCause::Contended
            }
        }
    }
}

/// Whether the fire's renewal attempt leaves this session holding the node
/// lease. `Ok(true)` is renewed outright. `Ok(false)` is ALSO renewed when
/// the claim is a pid-liveness claim - `claim acquire --pid` with no TTL,
/// the spawn-handover shape - whose `expires_at` is NULL: there is no
/// deadline for `renew` to extend, so the store answers `unchanged`
/// (`refreshed: false`) for a perfectly healthy one, and the anchored pid
/// IS the lease. Reads the claim once and applies the same verdict
/// `fno agent claim status` prints, so the answer is never a second
/// liveness opinion. A claim another holder owns, a dead or unprovable
/// pid, or an unreadable record reads not-held, exactly as before.
pub(super) fn renewal_holds(
    key: &str,
    holder: &str,
    outcome: &Result<bool, String>,
    root: Option<&std::path::Path>,
) -> bool {
    match outcome {
        Ok(true) => true,
        Ok(false) => {
            let Ok(path) = crate::claims::claim_path(key, root) else {
                return false;
            };
            match crate::claims::read_claim_file(&path) {
                Ok(rec) if rec.holder == holder && rec.expires_at.is_none() => {
                    crate::claim_commands::status_verdict(&rec).0 == crate::claims::ClaimState::Live
                }
                _ => false,
            }
        }
        Err(_) => false,
    }
}

/// Classify a declined renewal of this session's recorded claim pair.
/// `outcome` is what `renew` answered; `None` on either side means the
/// manifest recorded no pair, renew never ran, or the `unchanged` answer
/// still holds the lease (a pid-liveness claim), and the cause stays None.
pub(super) fn declined_cause(
    claim: Option<&(String, String)>,
    outcome: Option<&Result<bool, String>>,
) -> Option<RenewCause> {
    claim.zip(outcome).and_then(|((key, holder), res)| {
        if renewal_holds(key, holder, res, None) {
            None
        } else {
            Some(renew_cause(
                key,
                holder,
                res.as_ref().err().map(String::as_str),
                None,
            ))
        }
    })
}

/// The refusal a watching fire falls through to when it cannot idle, with
/// the `watch_refusal` row field naming the cause class.
pub(super) struct WatchRefusal {
    pub(super) reason: String,
    pub(super) kind: &'static str,
}

pub(super) fn idle_refusal(
    can_idle: bool,
    author_harness: Option<&str>,
    is_loop_run_child: bool,
    blocker_none: bool,
    unaddressed_findings: usize,
    claim_present: bool,
    lease_cause: Option<&RenewCause>,
) -> WatchRefusal {
    // Session-level causes outrank PR-level ones: a missing claim is
    // permanent for the life of the session (the manifest is write-once), so
    // no PR state can ever cure it. An unclaimed session with no PR must
    // read the no-claim refusal, never "not in an async wait class".
    let reason = if !can_idle {
        watching_harness_refusal(author_harness, is_loop_run_child)
    } else if !claim_present {
        NO_CLAIM_REFUSAL.to_string()
    } else if blocker_none && unaddressed_findings > 0 {
        format!(
            "watching ignored: {unaddressed_findings} unaddressed findings, this is not \
an async wait"
        )
    } else if blocker_none {
        "watching ignored: PR is not in an async wait class".to_string()
    } else if let Some(cause) = lease_cause.filter(|c| c.is_permanent()) {
        renewal_refusal(cause)
            .unwrap_or_else(|| "watching ignored: watch lease could not be renewed".to_string())
    } else {
        "watching ignored: watch lease could not be renewed".to_string()
    };
    let kind = if !can_idle {
        "harness"
    } else if !claim_present {
        "no_claim"
    } else if blocker_none {
        "not_async"
    } else {
        lease_cause.map(|c| c.as_str()).unwrap_or("write_failed")
    };
    WatchRefusal { reason, kind }
}

/// Whether a watching tag may idle with no claim lease behind it: a LOCAL
/// watch's wake is the session's own harness-tracked task exit, and an
/// unclaimed session holds no node claim a renewal could keep alive. A PR
/// wait (ci, review, merge_slot) never qualifies: the renewed lease is what
/// holds the node across the idle window. A local tag without a task id has
/// no task-exit wake at all, so it does not qualify either. The daemon's
/// expiry arm skips claimless sessions, so this idle has no daemon backstop;
/// the watcher's own hard time bound is the deadline.
pub(super) fn local_watch_without_claim(
    claim_present: bool,
    reason: &str,
    task_id: Option<&str>,
) -> bool {
    !claim_present && reason == "local" && task_id.is_some_and(|t| !t.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A holder-side fixture acquired under an explicit claims root.
    fn live_claim_opts(root: &std::path::Path) -> crate::claims::AcquireOpts {
        crate::claims::AcquireOpts {
            root: Some(root.to_path_buf()),
            ..Default::default()
        }
    }

    #[test]
    fn held_by_other_refusal_names_the_holder_and_the_remedy_without_the_arm_hint() {
        // AC3-HP: manifest names holder `a`, lockfile held by `b`.
        let td = tempfile::TempDir::new().unwrap();
        let _ = crate::claims::acquire(
            "node:x-b445t",
            "target-session:b",
            live_claim_opts(td.path()),
        );
        let cause = renew_cause("node:x-b445t", "target-session:a", None, Some(td.path()));
        let refusal = renewal_refusal(&cause).expect("held-by-other is permanent");
        assert!(refusal.contains("target-session:b"), "{refusal}");
        assert!(refusal.contains("fno do target start <node>"), "{refusal}");
    }

    /// A pid-anchored claim with no TTL, acquired the spawn-handover way.
    fn pid_liveness_opts(root: &std::path::Path) -> crate::claims::AcquireOpts {
        crate::claims::AcquireOpts {
            root: Some(root.to_path_buf()),
            pid: Some(std::process::id()),
            ..Default::default()
        }
    }

    #[test]
    fn pid_liveness_unchanged_renewal_holds_the_lease_for_a_live_pid() {
        //: a spawn-handover worker (claim acquire --pid, no TTL) arms the
        // sanctioned watcher and stops. Its claim carries a NULL expires_at,
        // so renew has no deadline to extend and answers unchanged
        // (refreshed: false) on every stop; the old classification read that
        // as Contended and refused every watching idle, 5+ consecutive. The
        // anchored pid IS the lease: unchanged plus a LIVE verdict is a held
        // lease, and the tag idles. The store answer itself must stay
        // unchanged - the interpretation changes, never the wire answer.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::TempDir::new().unwrap();
        let home = td.path().join("agents-home");
        std::fs::create_dir_all(&home).unwrap();
        let saved_home = std::env::var_os("FNO_AGENT_HOME");
        std::env::set_var("FNO_AGENT_HOME", &home);

        let _ = crate::claims::acquire(
            "node:x-p1dl",
            "target-session:me",
            pid_liveness_opts(td.path()),
        );
        let outcome =
            crate::claims::renew("node:x-p1dl", "target-session:me", 60_000, Some(td.path()));
        assert!(
            matches!(outcome, Ok(false)),
            "a ttl-less claim has no deadline to extend: {outcome:?}"
        );
        let holds = renewal_holds(
            "node:x-p1dl",
            "target-session:me",
            &outcome,
            Some(td.path()),
        );
        match saved_home {
            Some(v) => std::env::set_var("FNO_AGENT_HOME", v),
            None => std::env::remove_var("FNO_AGENT_HOME"),
        }
        assert!(holds, "live pid + unchanged must read as a held lease");
    }

    #[test]
    fn watch_target_never_reads_pr_zero_or_non_numbers() {
        // AC1-ERR: pr="0" is no PR, never PR zero.
        assert_eq!(watch_target("ci", Some("0")), ("ci", None));
        // AC1-EDGE: non-numbers and non-positives all read absent.
        assert_eq!(watch_target("review", Some("pending")), ("review", None));
        assert_eq!(watch_target("ci", Some("")), ("ci", None));
        assert_eq!(watch_target("ci", Some("-3")), ("ci", None));
        assert_eq!(watch_target("local", Some("0")), ("local", None));
    }
}
